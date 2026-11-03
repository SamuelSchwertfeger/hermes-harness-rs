"""Durable, fail-closed service launch journal independent of gateway admissions."""

import sqlite3
from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from pathlib import Path


class ServiceJournalError(RuntimeError):
    """A service identity cannot be safely recorded or transitioned."""


class ServiceState(StrEnum):
    """What the controller knows, not an inferred systemd state."""

    LAUNCH_UNCERTAIN = 'launch_uncertain'
    RUNNING_OBSERVED = 'running_observed'
    STOP_UNCERTAIN = 'stop_uncertain'
    STOPPED_OBSERVED = 'stopped_observed'


@dataclass(frozen=True, slots=True)
class ServiceRecord:
    """Durable service binding."""

    unit: str
    role: str
    plan_json: str
    state: ServiceState


class ServiceJournal:
    """SQLite records kept even when a launch or cancellation outcome is unknown."""

    def __init__(self, path: Path) -> None:
        if path.is_symlink():
            msg = 'Journal symlinks are not permitted'
            raise ServiceJournalError(msg)
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.path = path
        with self._connect() as connection:
            connection.execute(
                'CREATE TABLE IF NOT EXISTS services ('
                'unit TEXT PRIMARY KEY, role TEXT NOT NULL, '
                'plan_json TEXT NOT NULL, state TEXT NOT NULL)'
            )
        path.chmod(0o600)

    def _connect(self) -> sqlite3.Connection:
        connection = sqlite3.connect(self.path, timeout=5)
        connection.row_factory = sqlite3.Row
        return connection

    def reserve(self, *, unit: str, role: str, plan_json: str) -> ServiceRecord:
        """Fence an identity before any external side effects."""
        try:
            with self._connect() as connection:
                connection.execute(
                    'INSERT INTO services (unit, role, plan_json, state) VALUES (?, ?, ?, ?)',
                    (unit, role, plan_json, ServiceState.LAUNCH_UNCERTAIN),
                )
        except sqlite3.IntegrityError as exc:
            msg = 'Service identity already reserved; never relaunch'
            raise ServiceJournalError(msg) from exc
        record = self.get(unit)
        if record is None:
            msg = 'Reserved service disappeared'
            raise ServiceJournalError(msg)
        return record

    def get(self, unit: str) -> ServiceRecord | None:
        """Read without changing the durable record."""
        with self._connect() as connection:
            row = connection.execute('SELECT * FROM services WHERE unit = ?', (unit,)).fetchone()
        if row is None:
            return None
        return ServiceRecord(row['unit'], row['role'], row['plan_json'], ServiceState(row['state']))

    def transition(self, unit: str, *, expected: ServiceState, target: ServiceState) -> None:
        """Atomic compare-and-swap transition; no optimistic retries."""
        with self._connect() as connection:
            result = connection.execute(
                'UPDATE services SET state = ? WHERE unit = ? AND state = ?',
                (target, unit, expected),
            )
            if result.rowcount != 1:
                msg = 'Service transition refused; inspect durable state'
                raise ServiceJournalError(msg)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'ServiceJournal',
    'ServiceJournalError',
    'ServiceRecord',
    'ServiceState',
    'StrEnum',
    'dataclass',
    'sqlite3',
]
