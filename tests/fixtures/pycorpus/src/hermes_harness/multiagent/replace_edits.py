"""Content-addressed file replacements for trusted disposable Git worktrees.

Prepared replacements must be checked against exact previous content hashes.
This module does not execute model code or commit changes.
"""

import hashlib
import os
from dataclasses import dataclass
from pathlib import PurePosixPath
from typing import TYPE_CHECKING

from hermes_harness.git.worktrees import audit_worktree
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.git.worktrees import WorktreeAudit


@dataclass(frozen=True, slots=True)
class FileReplacement:
    path: str
    expected_sha256: str
    content: str


MAX_REPLACEMENTS = 16
MAX_REPLACEMENT_BYTES = 48 * 1024


def _replacement_path(name: str, owned_paths: tuple[str, ...], seen: set[str]) -> bool:
    parts = name.split('/')
    if not name or PurePosixPath(name).is_absolute():
        return False
    if '\\' in name or '\x00' in name:
        return False
    if any(piece in ('', '.', '..') for piece in parts) or parts[0] == '.git':
        return False
    if name in seen:
        return False
    return any(name == own or name.startswith(f'{own}/') for own in owned_paths)


def _prepare_replacement(  # noqa: PLR0913, PLR0917, PLR0915 - preserve audited boundary and established call contract
    root: Path, item: FileReplacement, owned_paths: tuple[str, ...], seen: set[str]
) -> tuple[Path, bytes]:
    name = item.path
    if not _replacement_path(name, owned_paths, seen):
        msg = 'Unsafe, unowned, or duplicate replacement path'
        raise WorkflowError(msg)
    seen.add(name)
    target = root / name
    if not target.resolve(strict=False).is_relative_to(root):
        msg = 'Replacement path escapes worktree'
        raise WorkflowError(msg)
    current = target
    while current != root:
        if current.is_symlink():
            msg = 'Replacement path traverses a symlink'
            raise WorkflowError(msg)
        current = current.parent
    if not target.is_file():
        msg = 'Replacement target must be an existing regular file'
        raise WorkflowError(msg)
    original = target.read_bytes()
    if hashlib.sha256(original).hexdigest() != item.expected_sha256:
        msg = 'Replacement preimage digest mismatch'
        raise WorkflowError(msg)
    data = item.content.encode('utf-8')
    if len(data) > MAX_REPLACEMENT_BYTES:
        msg = 'Replacement file exceeds size bound'
        raise WorkflowError(msg)
    return target, data


def _write_replacements(targets: list[tuple[Path, bytes]]) -> None:
    for target, data in targets:
        fd = os.open(target, os.O_WRONLY | os.O_TRUNC | getattr(os, 'O_NOFOLLOW', 0))
        with os.fdopen(fd, 'wb') as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())


def apply_replacements(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    workspace: Path,
    replacements: tuple[FileReplacement, ...],
    owned_paths: tuple[str, ...],
    baseline: str,
    *,
    authorized: bool = False,
) -> WorktreeAudit:
    """Apply reviewed exact-content replacements, rejecting stale state.

    A failed or interrupted write retains evidence, never silently retries.
    Caller must serialize filesystem changes and supply a disposable checkout.
    """
    if not authorized:
        msg = 'Explicit replacement authorization required'
        raise WorkflowError(msg)
    if not replacements or len(replacements) > MAX_REPLACEMENTS:
        msg = 'Invalid replacement count'
        raise WorkflowError(msg)
    if workspace.is_symlink() or not workspace.is_dir():
        msg = 'Expected real disposable worktree'
        raise WorkflowError(msg)
    root = workspace.resolve(strict=True)
    before = audit_worktree(root, baseline, owned_paths)
    if before.head != baseline or before.changed_paths:
        msg = 'Replacement requires unchanged baseline worktree'
        raise WorkflowError(msg)
    seen: set[str] = set()
    targets = [_prepare_replacement(root, item, owned_paths, seen) for item in replacements]
    _write_replacements(targets)
    after = audit_worktree(root, baseline, owned_paths)
    if set(after.changed_paths) != seen:
        msg = 'Replacement audit differs from intended changed paths'
        raise WorkflowError(msg)
    return after
