"""Read-only cross-ledger recovery report for supervised live jobs.

Reports are observations, never authority to launch, retry, approve, or merge.
Opening SQLite in read-only mode avoids silent database creation and mutation.
"""

import json
import sqlite3
from contextlib import closing
from dataclasses import asdict, dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from pathlib import Path


class LiveReportError(RuntimeError):
    """The operator cannot trust the supplied ledger set."""


@dataclass(frozen=True, slots=True)
class LedgerPaths:
    """Exact, explicitly selected on-host ledger files."""

    bindings: Path
    services: Path
    admissions: Path


@dataclass(frozen=True, slots=True)
class RoleReport:
    """Durable observations for a single task role."""

    role: str
    unit: str | None
    submission_id: str | None
    session_id: str | None
    service_state: str | None
    admission_state: str | None
    admission_id: str | None
    concerns: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class TaskReport:
    """Non-authoritative recovery summary with no RPC or state transitions."""

    task_id: str
    roles: tuple[RoleReport, ...]
    safe_to_dispatch: bool = False
    safe_to_retry: bool = False
    approved: bool = False

    def as_json(self) -> str:
        """Serialize only allowlisted fields; do not include prompts or credentials."""
        return json.dumps(asdict(self), indent=2)


def _open_readonly(path: Path) -> sqlite3.Connection:
    """Fail closed on missing paths, symlinks, directories, or SQLite failures."""
    if path.is_symlink() or not path.is_file():
        msg = 'Ledger must be an existing regular non-symlink file'
        raise LiveReportError(msg)
    # Reject symlinked ancestors too, without following untrusted directory links.
    if any(parent.is_symlink() for parent in path.parents):
        msg = 'Symlinked ledger parent is forbidden'
        raise LiveReportError(msg)
    try:
        connection = sqlite3.connect(path.absolute().as_uri() + '?mode=ro', uri=True)
        connection.row_factory = sqlite3.Row
    except sqlite3.Error as exc:
        msg = 'Cannot open operator ledger read-only'
        raise LiveReportError(msg) from exc
    return connection


def _rows(path: Path, query: str, params: tuple[str, ...]) -> list[sqlite3.Row]:
    try:
        with closing(_open_readonly(path)) as connection:
            return connection.execute(query, params).fetchall()
    except sqlite3.Error as exc:
        msg = 'Invalid or unreadable operator ledger schema'
        raise LiveReportError(msg) from exc


def _service_concerns(role: str, service: sqlite3.Row | None) -> list[str]:
    if service is None:
        return ['service_missing']
    concerns: list[str] = []
    if service['role'] != role:
        concerns.append('service_role_mismatch')
    if service['state'] != 'running_observed':
        concerns.append('service_not_recorded_running')
    return concerns


def _admission_concerns(binding: sqlite3.Row, admission: sqlite3.Row | None) -> list[str]:
    if admission is None:
        return ['admission_missing']
    concerns: list[str] = []
    if admission['session_id'] != binding['session_id']:
        concerns.append('session_mismatch')
    if admission['local_state'] in ('prepared', 'outcome_unknown'):
        concerns.append('submission_uncertain_or_pending')
    if admission['local_state'] not in (
        'prepared',
        'outcome_unknown',
        'admitted',
        'receipt_observed',
    ):
        concerns.append('admission_state_unrecognized')
    return concerns


def _role_report(
    role: str,
    binding: sqlite3.Row | None,
    observations: tuple[sqlite3.Row | None, sqlite3.Row | None],
) -> RoleReport:
    if binding is None:
        return RoleReport(role, None, None, None, None, None, None, ('binding_missing',))

    service, admission = observations
    concerns = _service_concerns(role, service)
    concerns.extend(_admission_concerns(binding, admission))
    # A previously active record is not current process liveness.
    concerns.append('live_process_not_verified')
    return RoleReport(
        role=role,
        unit=binding['unit'],
        submission_id=binding['submission_id'],
        session_id=binding['session_id'],
        service_state=service['state'] if service is not None else None,
        admission_state=admission['local_state'] if admission is not None else None,
        admission_id=admission['admission_id'] if admission is not None else None,
        concerns=tuple(concerns),
    )


def inspect_task(paths: LedgerPaths, task_id: str) -> TaskReport:
    """Join three durable ledgers without constructing mutable stores or doing RPC.

    The result cannot authorize execution even when all three ledgers agree.
    """
    if not task_id or not task_id.strip():
        msg = 'Nonblank task identity required'
        raise LiveReportError(msg)
    bindings = _rows(
        paths.bindings,
        'SELECT task_id, role, submission_id, session_id, unit FROM bindings '
        'WHERE task_id = ? ORDER BY role',
        (task_id,),
    )
    by_role = {row['role']: row for row in bindings}
    if len(by_role) != len(bindings) or set(by_role) - {'worker', 'reviewer'}:
        msg = 'Invalid task-role bindings'
        raise LiveReportError(msg)

    reports: list[RoleReport] = []
    for role in ('worker', 'reviewer'):
        binding = by_role.get(role)
        service = None
        admission = None
        if binding is not None:
            services = _rows(
                paths.services,
                'SELECT unit, role, state FROM services WHERE unit = ?',
                (binding['unit'],),
            )
            admissions = _rows(
                paths.admissions,
                'SELECT submission_id, session_id, admission_id, local_state '
                'FROM admissions WHERE submission_id = ?',
                (binding['submission_id'],),
            )
            if len(services) > 1 or len(admissions) > 1:
                msg = 'Duplicate durable identity'
                raise LiveReportError(msg)
            service = services[0] if services else None
            admission = admissions[0] if admissions else None
        reports.append(_role_report(role, binding, (service, admission)))
    return TaskReport(task_id, tuple(reports))


# Temporary exports for legacy module import compatibility.
__all__ = [
    'LedgerPaths',
    'LiveReportError',
    'RoleReport',
    'TaskReport',
    '_admission_concerns',
    '_open_readonly',
    '_role_report',
    '_rows',
    '_service_concerns',
    'asdict',
    'closing',
    'dataclass',
    'inspect_task',
    'json',
    'sqlite3',
]
