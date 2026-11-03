"""Build a deterministic read-only verification contract for an integration worktree.

No command execution is delegated here. A separate sandboxed verifier must
consume the contract and attach actual execution evidence.
"""

import hashlib
from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.git.worktrees import audit_worktree
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path


@dataclass(frozen=True, slots=True)
class VerificationManifest:
    baseline: str
    head: str
    changed_paths: tuple[str, ...]
    file_digests: tuple[tuple[str, str | None], ...]
    verified_by_tests: bool = False


def prepare_verification(
    workspace: Path, baseline: str, owned_paths: tuple[str, ...]
) -> VerificationManifest:
    """Reaudit and fingerprint changed files without running generated code."""
    if workspace.is_symlink() or not workspace.is_dir():
        msg = 'Verification requires a real worktree'
        raise WorkflowError(msg)
    root = workspace.resolve(strict=True)
    audit = audit_worktree(root, baseline, owned_paths)
    changes: list[tuple[str, str | None]] = []
    for path in audit.changed_paths:
        candidate = root / path
        if candidate.is_symlink() or not candidate.resolve(strict=False).is_relative_to(root):
            msg = 'Unsafe changed path during verification'
            raise WorkflowError(msg)
        if candidate.is_file():
            digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
        elif not candidate.exists():
            digest = None
        else:
            msg = 'Non-file in changed paths'
            raise WorkflowError(msg)
        changes.append((path, digest))
    return VerificationManifest(audit.baseline, audit.head, audit.changed_paths, tuple(changes))
