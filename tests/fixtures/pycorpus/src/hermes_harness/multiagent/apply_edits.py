"""Explicit, restricted application of admitted *new-file* proposals.

This is a filesystem mutation boundary for trusted, already-created disposable
worktrees. It is NOT a sandbox and never executes generated source or Git commits.
"""

import os
from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.git.worktrees import audit_worktree
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.git.worktrees import WorktreeAudit
    from hermes_harness.multiagent.model_edits import EditProposal, ProposedFile


@dataclass(frozen=True, slots=True)
class AppliedProposal:
    """Audit of an explicitly authorized, successful proposal application."""

    task_id: str
    paths: tuple[str, ...]
    audit: WorktreeAudit


def _target(root: Path, relative: str) -> Path:
    """Refuse symlink traversal and any path escaping the given worktree."""
    target = root / relative
    if not target.resolve(strict=False).is_relative_to(root):
        msg = 'Proposal path escapes worktree'
        raise WorkflowError(msg)
    current = target
    while current != root:
        if current.is_symlink():
            msg = 'Proposal path traverses a symlink'
            raise WorkflowError(msg)
        current = current.parent
    return target


def _prepare_new_file(
    root: Path, file: ProposedFile, owned_paths: tuple[str, ...]
) -> tuple[Path, bytes]:
    path = file.path
    if not any(path == own or path.startswith(f'{own}/') for own in owned_paths):
        msg = 'Proposal path exceeds assigned ownership'
        raise WorkflowError(msg)
    target = _target(root, path)
    if target.exists() or target.is_symlink():
        msg = 'Proposal may only create new files'
        raise WorkflowError(msg)
    return target, file.content.encode('utf-8')


def _write_new_files(root: Path, targets: list[tuple[Path, bytes]]) -> None:
    for target, payload in targets:
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        _target(root, str(target.relative_to(root)))
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, 'O_NOFOLLOW'):
            flags |= os.O_NOFOLLOW
        with os.fdopen(os.open(target, flags, 0o600), 'wb') as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())


def apply_new_files(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    workspace: Path,
    proposal: EditProposal,
    owned_paths: tuple[str, ...],
    baseline: str,
    *,
    authorized: bool = False,
) -> AppliedProposal:
    """Write only absent regular files and re-audit changed paths.

    Caller must supply a disposable worktree. Failed writes leave evidence
    in place for manual inspection; automatic replay is prohibited.
    """
    if not authorized:
        msg = 'Explicit proposal application authorization required'
        raise WorkflowError(msg)
    if workspace.is_symlink() or not workspace.is_dir():
        msg = 'Expected existing real disposable worktree'
        raise WorkflowError(msg)
    root = workspace.resolve(strict=True)
    before = audit_worktree(root, baseline, owned_paths)
    if before.changed_paths or before.head != baseline:
        msg = 'Proposal application requires clean worktree at baseline'
        raise WorkflowError(msg)
    targets = [_prepare_new_file(root, file, owned_paths) for file in proposal.files]
    _write_new_files(root, targets)
    after = audit_worktree(root, baseline, owned_paths)
    if set(after.changed_paths) != {file.path for file in proposal.files}:
        msg = 'Applied changes differ from approved proposal'
        raise WorkflowError(msg)
    return AppliedProposal(proposal.task_id, tuple(file.path for file in proposal.files), after)
