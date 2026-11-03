"""Durable role-to-service bindings; never infer authorization from a service name."""

import sqlite3
from dataclasses import dataclass
from typing import TYPE_CHECKING, Literal

from hermes_harness.services.live_service_journal import ServiceState

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.services.live_service_journal import ServiceJournal

Role = Literal['worker', 'reviewer']


class ExecutionBindingError(RuntimeError):
    """A role or service binding cannot be established safely."""


@dataclass(frozen=True, slots=True, kw_only=True)
class ExecutionBinding:
    """Immutable identity linking a task, gateway submission and systemd unit."""

    task_id: str
    role: Role
    submission_id: str
    session_id: str
    unit: str


class ExecutionBindingStore:
    """Durable binding registry with unique submissions, units and task roles."""

    def __init__(self, path: Path) -> None:
        if path.is_symlink():
            msg = 'Binding store symlinks are forbidden'
            raise ExecutionBindingError(msg)
        path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
        self.path = path
        with self._connect() as conn:
            conn.execute(
                'CREATE TABLE IF NOT EXISTS bindings ('
                'task_id TEXT NOT NULL, role TEXT NOT NULL, '
                'submission_id TEXT NOT NULL UNIQUE, session_id TEXT NOT NULL UNIQUE, '
                'unit TEXT NOT NULL UNIQUE, PRIMARY KEY (task_id, role))'
            )
        path.chmod(0o600)

    def _connect(self) -> sqlite3.Connection:
        return sqlite3.connect(self.path, timeout=5)

    def bind(self, binding: ExecutionBinding, journal: ServiceJournal) -> None:
        """Record intent before service launch or gateway RPC; never overwrite."""
        if binding.role not in ('worker', 'reviewer'):
            msg = 'Unknown role'
            raise ExecutionBindingError(msg)
        if not all((binding.task_id, binding.submission_id, binding.session_id, binding.unit)):
            msg = 'Blank binding identity'
            raise ExecutionBindingError(msg)
        if not binding.unit.startswith('hermes-job-'):
            msg = 'Unit outside authorized namespace'
            raise ExecutionBindingError(msg)
        if journal.get(binding.unit) is not None:
            msg = 'Service unit already used or uncertain'
            raise ExecutionBindingError(msg)
        try:
            with self._connect() as conn:
                conn.execute(
                    'INSERT INTO bindings VALUES (?, ?, ?, ?, ?)',
                    (
                        binding.task_id,
                        binding.role,
                        binding.submission_id,
                        binding.session_id,
                        binding.unit,
                    ),
                )
        except sqlite3.IntegrityError as exc:
            msg = 'Duplicate role, session, submission or unit'
            raise ExecutionBindingError(msg) from exc

    def get(self, task_id: str, role: Role) -> ExecutionBinding | None:
        """Read only; does not advance service state."""
        with self._connect() as conn:
            row = conn.execute(
                'SELECT task_id, role, submission_id, session_id, unit '
                'FROM bindings WHERE task_id = ? AND role = ?',
                (task_id, role),
            ).fetchone()
        if row is None:
            return None
        return ExecutionBinding(
            task_id=row[0],
            role=row[1],
            submission_id=row[2],
            session_id=row[3],
            unit=row[4],
        )

    def require_running(self, binding: ExecutionBinding, journal: ServiceJournal) -> None:
        """Require the exact recorded service to be observed running."""
        if self.get(binding.task_id, binding.role) != binding:
            msg = 'Gateway/service binding mismatch'
            raise ExecutionBindingError(msg)
        service = journal.get(binding.unit)
        if service is None or service.state != ServiceState.RUNNING_OBSERVED:
            msg = 'Service not durably observed running'
            raise ExecutionBindingError(msg)
        if service.role != binding.role:
            msg = 'Service role mismatch'
            raise ExecutionBindingError(msg)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'ExecutionBinding',
    'ExecutionBindingError',
    'ExecutionBindingStore',
    'Literal',
    'Role',
    'ServiceState',
    'dataclass',
    'sqlite3',
]
