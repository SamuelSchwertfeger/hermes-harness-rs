"""Durable, observation-only recovery for the supervised execution pipeline.

Never submits gateway work, starts a service, retries an uncertain request, or
approves a decision. An operator must separately authorize each live role.
"""

import hashlib
import json
import sqlite3
from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING

from hermes_harness.agents.supervised_review_handoff import verify_reviewer_handoff

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.agents.supervised_review_handoff import ReviewerHandoff
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.git.worktree_delegation import TaskWorkspace


class RecoveryError(RuntimeError):
    """A persisted supervised operation cannot safely advance."""


class Phase(StrEnum):
    """Monotonic evidence checkpoints; never an execution instruction."""

    PLANNED = 'planned'
    WORKER_ADMITTED = 'worker_admitted'
    WORKER_AUDITED = 'worker_audited'
    REVIEWER_ADMITTED = 'reviewer_admitted'
    PENDING_HUMAN = 'pending_human'
    HALTED = 'halted'


MAX_HALT_REASON_LENGTH = 256
SHA256_HEX_LENGTH = hashlib.sha256().digest_size * 2

PHASE_ORDER = (
    Phase.PLANNED,
    Phase.WORKER_ADMITTED,
    Phase.WORKER_AUDITED,
    Phase.REVIEWER_ADMITTED,
    Phase.PENDING_HUMAN,
)


@dataclass(frozen=True, slots=True, kw_only=True)
class RecoveryIdentity:
    """Stable task and gateway identities supplied by the operator."""

    task_id: str
    worker_submission_id: str
    reviewer_submission_id: str

    def __post_init__(self) -> None:
        ids = (self.task_id, self.worker_submission_id, self.reviewer_submission_id)
        if any(not value or not value.strip() for value in ids):
            msg = 'Nonblank identity fields required'
            raise RecoveryError(msg)
        if self.worker_submission_id == self.reviewer_submission_id:
            msg = 'Worker and reviewer must be independent submissions'
            raise RecoveryError(msg)


@dataclass(frozen=True, slots=True, kw_only=True)
class RecoveryCheckpoint:
    """A durable statement about evidence already inspected."""

    phase: Phase
    evidence_sha256: str
    halted_reason: str | None


class RecoveryLedger:
    """Single-writer-independent SQLite CAS journal; no automatic execution."""

    def __init__(self, path: Path) -> None:
        if path.is_symlink():
            msg = 'Recovery ledger symlink forbidden'
            raise RecoveryError(msg)
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.path = path
        with self._connect() as db:
            db.execute(
                'CREATE TABLE IF NOT EXISTS recovery ('
                'task_id TEXT PRIMARY KEY, worker_id TEXT NOT NULL, '
                'reviewer_id TEXT NOT NULL, phase TEXT NOT NULL, '
                'evidence_sha256 TEXT NOT NULL, halted_reason TEXT)'
            )
        path.chmod(0o600)

    def _connect(self) -> sqlite3.Connection:
        return sqlite3.connect(self.path, timeout=5.0)

    def initialize(self, identity: RecoveryIdentity) -> RecoveryCheckpoint:
        """Idempotent only for identical identities; no state reset."""
        with self._connect() as db:
            db.execute(
                'INSERT OR IGNORE INTO recovery VALUES (?, ?, ?, ?, ?, NULL)',
                (
                    identity.task_id,
                    identity.worker_submission_id,
                    identity.reviewer_submission_id,
                    Phase.PLANNED.value,
                    '',
                ),
            )
        return self.read(identity)

    def read(self, identity: RecoveryIdentity) -> RecoveryCheckpoint:
        with self._connect() as db:
            row = db.execute(
                'SELECT worker_id, reviewer_id, phase, evidence_sha256, halted_reason '
                'FROM recovery WHERE task_id = ?',
                (identity.task_id,),
            ).fetchone()
        if row is None or row[0:2] != (
            identity.worker_submission_id,
            identity.reviewer_submission_id,
        ):
            msg = 'Recovery identity missing or conflicts with persisted task'
            raise RecoveryError(msg)
        return RecoveryCheckpoint(phase=Phase(row[2]), evidence_sha256=row[3], halted_reason=row[4])

    def advance(
        self,
        identity: RecoveryIdentity,
        transition: tuple[Phase, Phase],
        evidence_sha256: str,
    ) -> RecoveryCheckpoint:
        """Commit exactly the next evidence phase; reject gaps and stale writers."""
        expected, target = transition
        current = self.read(identity)
        if current.phase != expected:
            msg = 'Stale checkpoint or halted operation; inspect before proceeding'
            raise RecoveryError(msg)
        if expected not in PHASE_ORDER or target not in PHASE_ORDER:
            msg = 'Halted operations cannot resume automatically'
            raise RecoveryError(msg)
        if PHASE_ORDER.index(target) != PHASE_ORDER.index(expected) + 1:
            msg = 'Recovery evidence cannot skip phases'
            raise RecoveryError(msg)
        if len(evidence_sha256) != SHA256_HEX_LENGTH:
            msg = 'Expected SHA256 evidence digest'
            raise RecoveryError(msg)
        with self._connect() as db:
            changed = db.execute(
                'UPDATE recovery SET phase=?, evidence_sha256=? '
                'WHERE task_id=? AND phase=? AND evidence_sha256=? AND halted_reason IS NULL',
                (
                    target.value,
                    evidence_sha256,
                    identity.task_id,
                    expected.value,
                    current.evidence_sha256,
                ),
            ).rowcount
        if changed != 1:
            msg = 'Concurrent checkpoint update rejected'
            raise RecoveryError(msg)
        return self.read(identity)

    def halt(self, identity: RecoveryIdentity, reason: str) -> RecoveryCheckpoint:
        """Durably freeze an operation on denial, cancellation or uncertainty."""
        if not reason.strip() or len(reason) > MAX_HALT_REASON_LENGTH:
            msg = 'Bounded nonempty halt reason required'
            raise RecoveryError(msg)
        self.read(identity)
        with self._connect() as db:
            db.execute(
                'UPDATE recovery SET phase=?, halted_reason=? WHERE task_id=? AND phase != ?',
                (Phase.HALTED.value, reason, identity.task_id, Phase.HALTED.value),
            )
        return self.read(identity)


def evidence_digest(document: object) -> str:
    """Digest deterministic non-secret evidence metadata, not prompt contents."""
    value = json.dumps(document, sort_keys=True, separators=(',', ':')).encode()
    return hashlib.sha256(value).hexdigest()


def inspect_admissions(identity: RecoveryIdentity, store: AdmissionStore) -> tuple[bool, bool]:
    """Read authoritative admission records without a gateway call or replay."""
    worker = store.get(identity.worker_submission_id)
    reviewer = store.get(identity.reviewer_submission_id)
    if reviewer is not None and (worker is None or not worker.admission_id):
        msg = 'Reviewer admission precedes known worker admission'
        raise RecoveryError(msg)
    return (
        worker is not None and worker.admission_id is not None,
        reviewer is not None and reviewer.admission_id is not None,
    )


def inspect_audit(task: TaskWorkspace, handoff: ReviewerHandoff) -> str:
    """Validate current Git audit before allowing human-gated reviewer admission."""
    verify_reviewer_handoff(task, handoff)
    return evidence_digest({
        'task_id': handoff.task_id,
        'worker_submission_id': handoff.worker_submission_id,
        'worker_admission_id': handoff.worker_admission_id,
        'audit_sha256': handoff.audit_sha256,
    })


def inspect_pending_decision(path: Path, identity: RecoveryIdentity) -> str:
    """Verify one-shot decision evidence is still pending and identity-bound."""
    if path.is_symlink() or not path.is_file():
        msg = 'Human decision request missing or unsafe'
        raise RecoveryError(msg)
    raw = path.read_bytes()
    document: object = json.loads(raw)
    if not isinstance(document, dict):
        msg = 'Invalid human decision request'
        raise RecoveryError(msg)
    matches_identity = all((
        document.get('task_id') == identity.task_id,
        document.get('worker_submission_id') == identity.worker_submission_id,
        document.get('reviewer_submission_id') == identity.reviewer_submission_id,
    ))
    is_pending = document.get('approved') is False and document.get('human_decision') == 'pending'
    if not matches_identity or not is_pending:
        msg = 'Decision record not pending or identity mismatch'
        raise RecoveryError(msg)
    return hashlib.sha256(raw).hexdigest()


def checkpoint_worker_admission(
    ledger: RecoveryLedger, identity: RecoveryIdentity, store: AdmissionStore
) -> RecoveryCheckpoint:
    """Record existing worker admission; never invokes a provider or gateway."""
    worker_known, _ = inspect_admissions(identity, store)
    if not worker_known:
        msg = 'Worker gateway admission is not durably established'
        raise RecoveryError(msg)
    worker = store.get(identity.worker_submission_id)
    if worker is None:
        msg = 'Worker admission disappeared'
        raise RecoveryError(msg)
    return ledger.advance(
        identity,
        (Phase.PLANNED, Phase.WORKER_ADMITTED),
        evidence_digest({
            'submission_id': worker.submission_id,
            'session_id': worker.session_id,
            'admission_id': worker.admission_id,
        }),
    )


def checkpoint_worker_audit(
    ledger: RecoveryLedger,
    identity: RecoveryIdentity,
    audit: tuple[TaskWorkspace, ReviewerHandoff],
) -> RecoveryCheckpoint:
    """Record independently verified Git audit and immutable reviewer handoff."""
    task, handoff = audit
    if (
        task.brief.task_id != identity.task_id
        or handoff.worker_submission_id != identity.worker_submission_id
    ):
        msg = 'Worktree and handoff identity do not match recovery task'
        raise RecoveryError(msg)
    digest = inspect_audit(task, handoff)
    return ledger.advance(identity, (Phase.WORKER_ADMITTED, Phase.WORKER_AUDITED), digest)


def checkpoint_reviewer_admission(
    ledger: RecoveryLedger, identity: RecoveryIdentity, store: AdmissionStore
) -> RecoveryCheckpoint:
    """Record existing reviewer admission with an independent gateway session."""
    worker_known, reviewer_known = inspect_admissions(identity, store)
    if not worker_known or not reviewer_known:
        msg = 'Both gateway admissions must be durably established'
        raise RecoveryError(msg)
    worker = store.get(identity.worker_submission_id)
    reviewer = store.get(identity.reviewer_submission_id)
    if worker is None or reviewer is None:
        msg = 'Reviewer admission must be independent of worker'
        raise RecoveryError(msg)
    if worker.session_id == reviewer.session_id or worker.admission_id == reviewer.admission_id:
        msg = 'Reviewer admission must be independent of worker'
        raise RecoveryError(msg)
    return ledger.advance(
        identity,
        (Phase.WORKER_AUDITED, Phase.REVIEWER_ADMITTED),
        evidence_digest({
            'submission_id': reviewer.submission_id,
            'session_id': reviewer.session_id,
            'admission_id': reviewer.admission_id,
        }),
    )


def checkpoint_pending_human(
    ledger: RecoveryLedger,
    identity: RecoveryIdentity,
    request_path: Path,
) -> RecoveryCheckpoint:
    """Only verify an already-written pending decision; never produce approval."""
    digest = inspect_pending_decision(request_path, identity)
    return ledger.advance(identity, (Phase.REVIEWER_ADMITTED, Phase.PENDING_HUMAN), digest)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_HALT_REASON_LENGTH',
    'PHASE_ORDER',
    'SHA256_HEX_LENGTH',
    'Phase',
    'RecoveryCheckpoint',
    'RecoveryError',
    'RecoveryIdentity',
    'RecoveryLedger',
    'StrEnum',
    'checkpoint_pending_human',
    'checkpoint_reviewer_admission',
    'checkpoint_worker_admission',
    'checkpoint_worker_audit',
    'dataclass',
    'evidence_digest',
    'hashlib',
    'inspect_admissions',
    'inspect_audit',
    'inspect_pending_decision',
    'json',
    'sqlite3',
    'verify_reviewer_handoff',
]
