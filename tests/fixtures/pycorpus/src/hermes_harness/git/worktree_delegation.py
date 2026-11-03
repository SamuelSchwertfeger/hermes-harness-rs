"""Offline delegation with Git worktree evidence; never launches a provider.

Worktree auditing is retrospective, not process isolation. Agents are injected
callables and MUST remain controlled offline doubles until OS isolation exists.
"""

import hashlib
import json
import os
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.core.delegation import (
    DelegationError,
    Phase,
    begin_reserved,
    delegate,
    phase,
    request_approval,
    review,
)
from hermes_harness.core.task_reservation import RecoveryState, fence, reconcile, reserve, task_lock
from hermes_harness.git.worktrees import WorktreeSpec, audit_worktree, create_worktree

if TYPE_CHECKING:
    from hermes_harness.core.delegation import (
        Brief,
        Finding,
        OfflineAgent,
    )
    from hermes_harness.git.worktrees import WorktreeAudit

PRIVATE_MODE = 0o600


@dataclass(frozen=True, slots=True)
class WorktreeBinding:
    """Immutable locally recorded worktree baseline and ownership."""

    task_id: str
    baseline: str
    branch: str
    owned_paths: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class TaskWorkspace:
    state_root: Path
    workspace: Path
    brief: Brief


def _canonical(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(',', ':')).encode()


def _write_once(path: Path, value: object) -> None:
    flags = os.O_CREAT | os.O_WRONLY | os.O_EXCL
    if hasattr(os, 'O_NOFOLLOW'):
        flags |= os.O_NOFOLLOW
    fd = os.open(path, flags, PRIVATE_MODE)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(_canonical(value) + b'\n')
        stream.flush()
        os.fsync(stream.fileno())


def _read(path: Path) -> dict[str, object]:
    if path.is_symlink():
        msg = 'Symlink evidence is forbidden'
        raise DelegationError(msg)
    value: object = json.loads(path.read_bytes())
    if not isinstance(value, dict):
        msg = 'Invalid evidence document'
        raise DelegationError(msg)
    return value


def _snapshot(workspace: Path, audit: WorktreeAudit) -> str:
    """Hash audited file contents, including deletions and untracked files."""
    chunks: list[tuple[str, str | None]] = []
    for name in audit.changed_paths:
        candidate = workspace / name
        if candidate.is_symlink():
            msg = 'Symlink in audited worktree'
            raise DelegationError(msg)
        if candidate.is_file():
            digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
        elif not candidate.exists():
            digest = None
        else:
            msg = 'Non-file in audited paths'
            raise DelegationError(msg)
        chunks.append((name, digest))
    return hashlib.sha256(_canonical(chunks)).hexdigest()


def _binding_path(root: Path, brief: Brief) -> Path:
    if root.is_symlink() or not root.is_dir():
        msg = 'Evidence root must be a real directory'
        raise DelegationError(msg)
    return root / brief.task_id / 'worktree-binding.json'


def _load_binding(root: Path, brief: Brief) -> WorktreeBinding:
    record = _read(_binding_path(root, brief))
    value = record.get('binding')
    if not isinstance(value, dict):
        msg = 'Invalid worktree binding'
        raise DelegationError(msg)
    digest = hashlib.sha256(_canonical(value)).hexdigest()
    if record.get('sha256') != digest:
        msg = 'Worktree binding digest mismatch'
        raise DelegationError(msg)
    task_id = value.get('task_id')
    baseline = value.get('baseline')
    branch = value.get('branch')
    paths = value.get('owned_paths')
    if not isinstance(task_id, str) or not isinstance(baseline, str) or not isinstance(branch, str):
        msg = 'Invalid worktree binding identity'
        raise DelegationError(msg)
    if not isinstance(paths, list) or any(not isinstance(path, str) for path in paths):
        msg = 'Invalid worktree ownership paths'
        raise DelegationError(msg)
    binding = WorktreeBinding(task_id, baseline, branch, tuple(paths))
    if binding.task_id != brief.task_id or binding.owned_paths != brief.owned_paths:
        msg = 'Worktree scope disagrees with task brief'
        raise DelegationError(msg)
    return binding


def prepare_worktree(task: TaskWorkspace, repo: Path) -> WorktreeBinding:
    """Create one worktree and begin its durable task, without dispatching agents.

    If evidence creation fails, the worktree is intentionally retained for manual
    inspection; never clean up automatically after an ambiguous write.
    """
    root, brief = task.state_root, task.brief
    spec = WorktreeSpec(brief.task_id, f'harness/{brief.task_id}', brief.owned_paths)
    reservation = reserve(task, repo)
    fence(task, 'preparing')
    created = create_worktree(repo, task.workspace, spec)
    initial = audit_worktree(created, 'HEAD', brief.owned_paths)
    if initial.changed_paths:
        msg = 'New worktree is unexpectedly dirty'
        raise DelegationError(msg)
    if initial.head != reservation.baseline:
        msg_0 = 'Worktree baseline differs from reserved source'
        raise DelegationError(msg_0)
    binding = WorktreeBinding(brief.task_id, initial.head, spec.branch, brief.owned_paths)
    begin_reserved(root, brief)
    value = asdict(binding)
    _write_once(
        _binding_path(root, brief),
        {'binding': value, 'sha256': hashlib.sha256(_canonical(value)).hexdigest()},
    )
    fence(task, 'prepared')
    return binding


def delegate_in_worktree(task: TaskWorkspace, worker: OfflineAgent) -> Finding:
    """Call a controlled offline double, then record independent Git audit evidence."""
    root, workspace, brief = task.state_root, task.workspace, task.brief
    binding = _load_binding(root, brief)
    if phase(root, brief) is not Phase.CREATED:
        msg = 'Worker may only run once'
        raise DelegationError(msg)
    with task_lock(root, brief.task_id):
        if reconcile(task) is not RecoveryState.PREPARED:
            msg_0 = 'Dispatch requires prepared reservation without prior dispatch'
            raise DelegationError(msg_0)
        fence(task, 'dispatch')
    result = delegate(root, brief, worker)
    audit = audit_worktree(workspace, binding.baseline, binding.owned_paths)
    _write_once(
        root / brief.task_id / 'worktree-audit.json',
        {'audit': asdict(audit), 'content_sha256': _snapshot(workspace, audit)},
    )
    return result


def verified_audit(task: TaskWorkspace) -> WorktreeAudit:
    """Re-audit current worktree; reject stale or altered evidence."""
    root, workspace, brief = task.state_root, task.workspace, task.brief
    binding = _load_binding(root, brief)
    if reconcile(task) is not RecoveryState.WORKER_RECORDED:
        msg = 'Incomplete or uncertain dispatch evidence'
        raise DelegationError(msg)
    if phase(root, brief) is Phase.CREATED:
        msg = 'Worker evidence is missing'
        raise DelegationError(msg)
    stored = _read(root / brief.task_id / 'worktree-audit.json')
    current = audit_worktree(workspace, binding.baseline, binding.owned_paths)
    current_record = {'audit': asdict(current), 'content_sha256': _snapshot(workspace, current)}
    if current.baseline != binding.baseline or _canonical(stored) != _canonical(current_record):
        msg = 'Worktree evidence changed after worker audit'
        raise DelegationError(msg)
    return current


def review_worktree(task: TaskWorkspace, reviewer: OfflineAgent) -> Finding:
    """Require independent Git audit before admitting separate reviewer output."""
    verified_audit(task)
    with task_lock(task.state_root, task.brief.task_id):
        if (task.state_root / task.brief.task_id / 'review_dispatch.json').exists():
            msg = 'Reviewer already dispatched or uncertain'
            raise DelegationError(msg)
        fence(task, 'review_dispatch')
    return review(task.state_root, task.brief, reviewer)


def request_worktree_approval(task: TaskWorkspace) -> Path:
    """Require unchanged audited evidence before recording unapproved request."""
    verified_audit(task)
    return request_approval(task.state_root, task.brief)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'PRIVATE_MODE',
    'DelegationError',
    'Path',
    'Phase',
    'RecoveryState',
    'TaskWorkspace',
    'WorktreeBinding',
    'WorktreeSpec',
    '_binding_path',
    '_canonical',
    '_load_binding',
    '_read',
    '_snapshot',
    '_write_once',
    'asdict',
    'audit_worktree',
    'begin_reserved',
    'create_worktree',
    'dataclass',
    'delegate',
    'delegate_in_worktree',
    'fence',
    'hashlib',
    'json',
    'os',
    'phase',
    'prepare_worktree',
    'reconcile',
    'request_approval',
    'request_worktree_approval',
    'reserve',
    'review',
    'review_worktree',
    'task_lock',
    'verified_audit',
]
