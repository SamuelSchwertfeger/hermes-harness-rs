"""Offline CI request anchored to an immutable Git commit and tree.

A request is not a CI result, attestation, authorization, or approval. It
cannot authenticate external CI and never executes tests or remote commands.
"""

import hashlib
from dataclasses import asdict, dataclass
from typing import TYPE_CHECKING

from hermes_harness.core.delegation import DelegationError, Phase, phase
from hermes_harness.core.task_reservation import RecoveryState, load, reconcile, task_lock
from hermes_harness.git.worktree_delegation import (
    _canonical,
    _read,
    _snapshot,
    _write_once,
)
from hermes_harness.git.worktrees import _git, audit_worktree

if TYPE_CHECKING:
    from hermes_harness.core.task_reservation import Reservation
    from hermes_harness.git.worktree_delegation import TaskWorkspace
    from hermes_harness.git.worktrees import WorktreeAudit


@dataclass(frozen=True, slots=True)
class CIRequest:
    task_id: str
    brief_digest: str
    baseline: str
    commit: str
    tree: str
    branch: str
    changed_paths: tuple[str, ...]
    content_sha256: str


def _validate_reviewed_snapshot(
    task: TaskWorkspace, record: Reservation, audit: WorktreeAudit
) -> str:
    """Require the committed candidate to match the reviewed worker evidence."""
    stored = _read(task.state_root / task.brief.task_id / 'worktree-audit.json')
    old_audit = stored.get('audit')
    if not isinstance(old_audit, dict):
        msg = 'Committed content does not match reviewed worker snapshot'
        raise DelegationError(msg)
    if (
        old_audit.get('changed_paths') != list(audit.changed_paths)
        or old_audit.get('baseline') != record.baseline
    ):
        msg = 'Committed content does not match reviewed worker snapshot'
        raise DelegationError(msg)
    digest = _snapshot(task.workspace, audit)
    if stored.get('content_sha256') != digest:
        msg = 'Committed content does not match reviewed worker snapshot'
        raise DelegationError(msg)
    return digest


def _validate_clean_branch(task: TaskWorkspace, record: Reservation, commit: str) -> None:
    """Require the reserved branch and an unmodified, committed working tree."""
    if (
        task.workspace.is_symlink()
        or task.workspace.resolve(strict=True) != task.workspace.absolute()
    ):
        msg = 'Noncanonical CI worktree'
        raise DelegationError(msg)
    if _git(task.workspace, 'status', '--porcelain', '--untracked-files=all'):
        msg = 'CI candidate must be committed and clean'
        raise DelegationError(msg)
    if (
        record.branch != _git(task.workspace, 'symbolic-ref', '--short', 'HEAD')
        or commit == record.baseline
    ):
        msg = 'CI candidate must be on reserved branch with new commit'
        raise DelegationError(msg)


def _candidate(task: TaskWorkspace) -> CIRequest:
    """Inspect reviewed committed work, without changing Git or disk state."""
    record = load(task)
    if reconcile(task) is not RecoveryState.WORKER_RECORDED:
        msg = 'Worker dispatch evidence incomplete or uncertain'
        raise DelegationError(msg)
    if phase(task.state_root, task.brief) not in (Phase.REVIEWED, Phase.AWAITING_APPROVAL):
        msg = 'Independent offline review required'
        raise DelegationError(msg)
    directory = task.state_root / task.brief.task_id
    if not (directory / 'review_dispatch.json').is_file():
        msg = 'Reviewer dispatch fence missing'
        raise DelegationError(msg)
    commit = _git(task.workspace, 'rev-parse', 'HEAD')
    _validate_clean_branch(task, record, commit)
    tree = _git(task.workspace, 'rev-parse', 'HEAD^{tree}')
    audit = audit_worktree(task.workspace, record.baseline, task.brief.owned_paths)
    digest = _validate_reviewed_snapshot(task, record, audit)
    return CIRequest(
        record.task_id,
        record.brief_digest,
        record.baseline,
        commit,
        tree,
        record.branch,
        audit.changed_paths,
        digest,
    )


def create_ci_request(task: TaskWorkspace) -> CIRequest:
    """Create a non-overwriting request; never launch CI or approve work."""
    with task_lock(task.state_root, task.brief.task_id):
        result = _candidate(task)
        path = task.state_root / task.brief.task_id / 'ci-request.json'
        payload = asdict(result)
        _write_once(
            path, {'candidate': payload, 'sha256': hashlib.sha256(_canonical(payload)).hexdigest()}
        )
        return result


def inspect_ci_request(task: TaskWorkspace) -> CIRequest:
    """Recheck saved request against local Git HEAD and reviewed content, read-only."""
    path = task.state_root / task.brief.task_id / 'ci-request.json'
    stored = _read(path)
    value = stored.get('candidate')
    if not isinstance(value, dict):
        msg = 'Malformed CI request'
        raise DelegationError(msg)
    if hashlib.sha256(_canonical(value)).hexdigest() != stored.get('sha256'):
        msg = 'CI request checksum mismatch'
        raise DelegationError(msg)
    try:
        expected = CIRequest(
            value['task_id'],
            value['brief_digest'],
            value['baseline'],
            value['commit'],
            value['tree'],
            value['branch'],
            tuple(value['changed_paths']),
            value['content_sha256'],
        )
    except (KeyError, TypeError, ValueError) as exc:
        msg = 'Malformed CI request contract'
        raise DelegationError(msg) from exc
    if _candidate(task) != expected:
        msg = 'CI request no longer matches local candidate'
        raise DelegationError(msg)
    return expected


# Temporary exports for legacy module import compatibility.
__all__ = [
    'CIRequest',
    'DelegationError',
    'Phase',
    'RecoveryState',
    '_candidate',
    '_canonical',
    '_git',
    '_read',
    '_snapshot',
    '_validate_clean_branch',
    '_validate_reviewed_snapshot',
    '_write_once',
    'asdict',
    'audit_worktree',
    'create_ci_request',
    'dataclass',
    'hashlib',
    'inspect_ci_request',
    'load',
    'phase',
    'reconcile',
    'task_lock',
]
