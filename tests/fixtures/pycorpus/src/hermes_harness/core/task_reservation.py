"""Offline durable reservation and read-only task recovery.

A dispatch fence is deliberately irreversible: even a crash before invoking an
agent requires manual resolution. Local records are not authenticated evidence.
"""

import fcntl
import hashlib
import json
import os
from contextlib import contextmanager
from dataclasses import asdict, dataclass
from enum import StrEnum
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.core.delegation import DelegationError, _digest, _validate
from hermes_harness.git.worktrees import WorktreeError, _git

if TYPE_CHECKING:
    from collections.abc import Iterator

    from hermes_harness.git.worktree_delegation import TaskWorkspace


class RecoveryState(StrEnum):
    RESERVED = 'reserved'
    PREPARING = 'preparing_uncertain'
    PREPARED = 'prepared'
    DISPATCH_UNCERTAIN = 'dispatch_uncertain'
    WORKER_RECORDED = 'worker_recorded'
    MANUAL_INTERVENTION = 'manual_intervention'


@dataclass(frozen=True, slots=True)
class Reservation:
    task_id: str
    brief_digest: str
    source_repo: str
    workspace: str
    baseline: str
    branch: str


def _json(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode() + b'\n'


def _atomic_new(path: Path, value: object) -> None:
    flags = os.O_CREAT | os.O_EXCL | os.O_WRONLY
    if hasattr(os, 'O_NOFOLLOW'):
        flags |= os.O_NOFOLLOW
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(_json(value))
        stream.flush()
        os.fsync(stream.fileno())
    directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


@contextmanager
def task_lock(root: Path, task_id: str) -> Iterator[None]:
    """Interprocess lock protecting reservation creation and stage fences."""
    if root.is_symlink() or not root.is_dir():
        msg = 'State root must be a real directory'
        raise DelegationError(msg)
    locks = root / '.reservation-locks'
    if locks.is_symlink():
        msg = 'Symlink lock directory'
        raise DelegationError(msg)
    locks.mkdir(mode=0o700, exist_ok=True)
    if locks.is_symlink():
        msg = 'Symlink lock directory'
        raise DelegationError(msg)
    fd = os.open(locks / f'{task_id}.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(fd, fcntl.LOCK_UN)
        os.close(fd)


def _safe_workspace(task: TaskWorkspace, repo: Path) -> tuple[Path, Path]:
    _validate(task.brief)
    root = task.state_root.resolve(strict=True)
    source = repo.resolve(strict=True)
    target = task.workspace.absolute()
    if target.is_symlink() or target.resolve(strict=False) != target:
        msg = 'Workspace path includes symlink or noncanonical components'
        raise DelegationError(msg)
    if target == source or target.is_relative_to(source) or source.is_relative_to(target):
        msg = 'Workspace and source repository overlap'
        raise DelegationError(msg)
    if target.is_relative_to(root) or root.is_relative_to(target):
        msg = 'Workspace and evidence root overlap'
        raise DelegationError(msg)
    if _git(source, 'rev-parse', '--show-toplevel') != str(source):
        msg = 'Source must be Git root'
        raise DelegationError(msg)
    if _git(source, 'status', '--porcelain'):
        msg = 'Source must be clean'
        raise DelegationError(msg)
    return source, target


def reserve(task: TaskWorkspace, repo: Path) -> Reservation:
    """Persist identity and baseline BEFORE performing Git worktree mutations."""
    source, target = _safe_workspace(task, repo)
    brief = task.brief
    with task_lock(task.state_root, brief.task_id):
        task_dir = task.state_root / brief.task_id
        if target.exists() or target.is_symlink():
            msg = 'Workspace already exists'
            raise WorktreeError(msg)
        if task_dir.exists() or task_dir.is_symlink():
            msg = 'Task ID already reserved'
            raise DelegationError(msg)
        baseline = _git(source, 'rev-parse', 'HEAD')
        record = Reservation(
            brief.task_id,
            _digest(brief),
            str(source),
            str(target),
            baseline,
            f'harness/{brief.task_id}',
        )
        task_dir.mkdir(mode=0o700)
        _atomic_new(task_dir / 'reservation.json', asdict(record))
        return record


def load(task: TaskWorkspace) -> Reservation:
    path = task.state_root / task.brief.task_id / 'reservation.json'
    if path.is_symlink() or path.parent.is_symlink():
        msg = 'Symlink reservation evidence'
        raise DelegationError(msg)
    try:
        value = json.loads(path.read_bytes())
        record = Reservation(**value)
    except (OSError, ValueError, TypeError, KeyError) as exc:
        msg = 'Reservation missing or malformed'
        raise DelegationError(msg) from exc
    expected_identity = (
        record.task_id == task.brief.task_id
        and record.brief_digest == _digest(task.brief)
        and record.workspace == str(task.workspace.absolute())
    )
    expected_branch = record.branch == f'harness/{task.brief.task_id}'
    git_oid_length = 40
    baseline_valid = len(record.baseline) == git_oid_length and all(
        ch in '0123456789abcdef' for ch in record.baseline
    )
    if not expected_identity or not expected_branch or not baseline_valid:
        msg = 'Reservation identity or digest mismatch'
        raise DelegationError(msg)
    return record


def fence(task: TaskWorkspace, stage: str) -> None:
    """Record an irreversible side-effect boundary, under the task lock."""
    if stage not in ('preparing', 'prepared', 'dispatch', 'review_dispatch'):
        msg = 'Unknown reservation stage'
        raise DelegationError(msg)
    load(task)
    _atomic_new(task.state_root / task.brief.task_id / f'{stage}.json', {'stage': stage})


STAGE_NAMES = ('preparing', 'prepared', 'dispatch', 'review_dispatch')


def _stage_evidence(directory: Path) -> dict[str, bool] | None:
    """Return stage presence; malformed or out-of-order markers fail closed."""
    present: dict[str, bool] = {}
    for name in STAGE_NAMES:
        marker = directory / f'{name}.json'
        if marker.is_symlink():
            return None
        if marker.exists() and json.loads(marker.read_bytes()) != {'stage': name}:
            return None
        present[name] = marker.exists()
    if present['prepared'] and not present['preparing']:
        return None
    if present['dispatch'] and not present['prepared']:
        return None
    if present['review_dispatch'] and not present['dispatch']:
        return None
    return present


def _worker_state(directory: Path, worker_id: str) -> RecoveryState:
    worker = directory / 'worker.json'
    audit = directory / 'worktree-audit.json'
    if worker.is_symlink() or audit.is_symlink():
        return RecoveryState.MANUAL_INTERVENTION
    if not worker.is_file() or not audit.is_file():
        return RecoveryState.DISPATCH_UNCERTAIN
    worker_data = json.loads(worker.read_bytes())
    audit_data = json.loads(audit.read_bytes())
    if not isinstance(worker_data, dict) or not isinstance(audit_data, dict):
        return RecoveryState.MANUAL_INTERVENTION
    valid_worker = worker_data.get('agent_id') == worker_id and isinstance(
        worker_data.get('text'), str
    )
    valid_audit = isinstance(audit_data.get('audit'), dict) and isinstance(
        audit_data.get('content_sha256'), str
    )
    if valid_worker and valid_audit:
        return RecoveryState.WORKER_RECORDED
    return RecoveryState.MANUAL_INTERVENTION


def _prepared_state(directory: Path, record: Reservation) -> RecoveryState:
    brief_path = directory / 'brief.json'
    binding_path = directory / 'worktree-binding.json'
    if any(path.is_symlink() or not path.is_file() for path in (brief_path, binding_path)):
        return RecoveryState.MANUAL_INTERVENTION
    workspace = Path(record.workspace)
    if workspace.is_symlink() or not workspace.is_dir():
        return RecoveryState.MANUAL_INTERVENTION
    baseline_matches = _git(workspace, 'rev-parse', 'HEAD') == record.baseline
    branch_matches = _git(workspace, 'symbolic-ref', '--short', 'HEAD') == record.branch
    if baseline_matches and branch_matches:
        return RecoveryState.PREPARED
    return RecoveryState.MANUAL_INTERVENTION


def _reconcile_state(
    task: TaskWorkspace, record: Reservation, present: dict[str, bool]
) -> RecoveryState:
    directory = task.state_root / task.brief.task_id
    if present['dispatch']:
        return _worker_state(directory, task.brief.worker_id)
    if present['prepared']:
        return _prepared_state(directory, record)
    if present['preparing']:
        return RecoveryState.PREPARING
    if Path(record.workspace).exists():
        return RecoveryState.MANUAL_INTERVENTION
    return RecoveryState.RESERVED


def reconcile(task: TaskWorkspace) -> RecoveryState:
    """Read-only inspection, without replaying Git or agent side effects."""
    record = load(task)
    directory = task.state_root / task.brief.task_id
    try:
        present = _stage_evidence(directory)
        if present is None:
            return RecoveryState.MANUAL_INTERVENTION
        return _reconcile_state(task, record, present)
    except OSError, ValueError, TypeError, WorktreeError:
        return RecoveryState.MANUAL_INTERVENTION


def reservation_digest(record: Reservation) -> str:
    """Local checksum for display; not a trusted signature."""
    return hashlib.sha256(_json(asdict(record))).hexdigest()
