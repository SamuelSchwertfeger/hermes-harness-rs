"""Durable, local-only records for reconciling Hermes gateway admissions."""

import sqlite3
from dataclasses import dataclass
from datetime import UTC, datetime
from enum import StrEnum
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.gateway.gateway_identity import Receipt, Submission


class AdmissionStoreError(RuntimeError):
    """The local admission ledger cannot safely apply the requested operation."""


FILE_MODE_MASK = 0o777
PRIVATE_FILE_MODE = 0o600
MAX_LIST_LIMIT = 1_000


class LocalState(StrEnum):
    """Harness-side knowledge; deliberately distinct from Hermes status values."""

    PREPARED = 'prepared'
    OUTCOME_UNKNOWN = 'outcome_unknown'
    ADMITTED = 'admitted'
    RECEIPT_OBSERVED = 'receipt_observed'


@dataclass(frozen=True, slots=True)
class AdmissionRecord:
    """Persisted request identity and the latest observed gateway receipt."""

    submission_id: str
    session_id: str
    admission_id: str | None
    local_state: LocalState
    gateway_status: str | None
    execution_generation: int | None
    updated_at: str


class AdmissionStore:
    """SQLite-backed ledger with unique identities and atomic updates.

    The ledger never submits work and never interprets opaque gateway status strings
    as proof that execution has stopped. Callers must reconcile unknown outcomes
    using the stable submission ID and the verified Hermes API.
    """

    def __init__(self, path: Path) -> None:
        self._path = path
        self._path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self._path.is_symlink():
            msg = 'Admission database path must not be a symbolic link'
            raise AdmissionStoreError(msg)
        with self._connect() as connection:
            connection.execute(
                """
                CREATE TABLE IF NOT EXISTS admissions (
                    submission_id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    admission_id TEXT UNIQUE,
                    local_state TEXT NOT NULL,
                    gateway_status TEXT,
                    execution_generation INTEGER,
                    updated_at TEXT NOT NULL
                )
                """
            )
        self._path.chmod(PRIVATE_FILE_MODE)

    def _connect(self) -> sqlite3.Connection:
        connection = sqlite3.connect(self._path, timeout=5.0)
        connection.row_factory = sqlite3.Row
        return connection

    @staticmethod
    def _now() -> str:
        return datetime.now(UTC).isoformat()

    @staticmethod
    def _record(row: sqlite3.Row | None) -> AdmissionRecord | None:
        if row is None:
            return None
        return AdmissionRecord(
            submission_id=row['submission_id'],
            session_id=row['session_id'],
            admission_id=row['admission_id'],
            local_state=LocalState(row['local_state']),
            gateway_status=row['gateway_status'],
            execution_generation=row['execution_generation'],
            updated_at=row['updated_at'],
        )

    def prepare(self, submission: Submission) -> AdmissionRecord:
        """Persist intent before network I/O; duplicate IDs are never overwritten."""
        if not submission.session_id.strip() or not submission.submission_id.strip():
            msg = 'Session and submission IDs must be nonblank'
            raise ValueError(msg)
        timestamp = self._now()
        try:
            with self._connect() as connection:
                connection.execute(
                    """
                    INSERT INTO admissions (
                        submission_id, session_id, admission_id, local_state,
                        gateway_status, execution_generation, updated_at
                    ) VALUES (?, ?, NULL, ?, NULL, NULL, ?)
                    """,
                    (
                        submission.submission_id,
                        submission.session_id,
                        LocalState.PREPARED.value,
                        timestamp,
                    ),
                )
        except sqlite3.IntegrityError as exc:
            msg = 'Submission ID already exists; refusing to overwrite its record'
            raise AdmissionStoreError(msg) from exc
        record = self.get(submission.submission_id)
        if record is None:
            msg = 'Admission record disappeared after insertion'
            raise AdmissionStoreError(msg)
        return record

    def mark_outcome_unknown(self, submission_id: str) -> AdmissionRecord:
        """Record uncertainty without deleting any admission identity already known."""
        with self._connect() as connection:
            cursor = connection.execute(
                """
                UPDATE admissions SET local_state = ?, updated_at = ?
                WHERE submission_id = ?
                """,
                (LocalState.OUTCOME_UNKNOWN.value, self._now(), submission_id),
            )
            if cursor.rowcount != 1:
                msg = f'Unknown submission ID: {submission_id}'
                raise AdmissionStoreError(msg)
        record = self.get(submission_id)
        if record is None:
            msg = 'Admission record disappeared after update'
            raise AdmissionStoreError(msg)
        return record

    def record_receipt(
        self,
        submission_id: str,
        receipt: Receipt,
        *,
        reconciled: bool = False,
    ) -> AdmissionRecord:
        """Persist a validated receipt and reject changes to an established ID.

        Set ``reconciled=True`` only when the receipt was obtained through an
        authoritative lookup, not by resubmitting a request whose outcome was lost.
        """
        with self._connect() as connection:
            row = connection.execute(
                'SELECT * FROM admissions WHERE submission_id = ?', (submission_id,)
            ).fetchone()
            if row is None:
                msg = f'Unknown submission ID: {submission_id}'
                raise AdmissionStoreError(msg)
            current_admission_id = row['admission_id']
            if current_admission_id is not None and current_admission_id != receipt.admission_id:
                msg = 'Receipt admission ID conflicts with the durable record'
                raise AdmissionStoreError(msg)
            state = LocalState.RECEIPT_OBSERVED if reconciled else LocalState.ADMITTED
            try:
                connection.execute(
                    """
                    UPDATE admissions
                    SET admission_id = ?, local_state = ?, gateway_status = ?,
                        execution_generation = ?, updated_at = ?
                    WHERE submission_id = ?
                    """,
                    (
                        receipt.admission_id,
                        state.value,
                        receipt.status,
                        receipt.execution_generation,
                        self._now(),
                        submission_id,
                    ),
                )
            except sqlite3.IntegrityError as exc:
                msg = 'Admission ID is already assigned to another submission'
                raise AdmissionStoreError(msg) from exc
        record = self.get(submission_id)
        if record is None:
            msg = 'Admission record disappeared after receipt update'
            raise AdmissionStoreError(msg)
        return record

    def get(self, submission_id: str) -> AdmissionRecord | None:
        """Return one record without changing its state."""
        with self._connect() as connection:
            row = connection.execute(
                'SELECT * FROM admissions WHERE submission_id = ?', (submission_id,)
            ).fetchone()
        return self._record(row)

    def list_records(self, *, limit: int = 100) -> tuple[AdmissionRecord, ...]:
        """List recent records for operator reconciliation, newest update first."""
        if not 1 <= limit <= MAX_LIST_LIMIT:
            msg = 'limit must be between 1 and 1000'
            raise ValueError(msg)
        with self._connect() as connection:
            rows = connection.execute(
                'SELECT * FROM admissions ORDER BY updated_at DESC, submission_id LIMIT ?',
                (limit,),
            ).fetchall()
        return tuple(record for row in rows if (record := self._record(row)) is not None)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'FILE_MODE_MASK',
    'MAX_LIST_LIMIT',
    'PRIVATE_FILE_MODE',
    'UTC',
    'AdmissionRecord',
    'AdmissionStore',
    'AdmissionStoreError',
    'LocalState',
    'StrEnum',
    'dataclass',
    'datetime',
    'sqlite3',
]
