"""Read-only integrity verification of a human-authorized reviewed worktree.

This gate never runs generated code. Its durable evidence is an observation,
not an approval or a claim that tests passed.
"""

import hashlib
import json
import os
import re
from typing import TYPE_CHECKING

from hermes_harness.git.worktrees import WorktreeError, audit_worktree
from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.workflows import _git, repository
from hermes_harness.native.worktree_apply import _admitted_proposal, _read_receipt

if TYPE_CHECKING:
    from pathlib import Path

_MAX_EVIDENCE_BYTES = 128 * 1024


def verify_applied(  # noqa: C901, PLR0912, PLR0913, PLR0914, PLR0915 - integrity gate
    *, repo: Path, receipt: Path, workspace: Path, evidence: Path, authorized: bool = False
) -> dict[str, object]:
    """Re-audit exact owned paths and bytes; persist a non-overwriting receipt."""
    if not authorized:
        msg = 'Explicit authorization to record worktree verification is required'
        raise WorkflowError(msg)
    root = repository(repo)
    record = _read_receipt(receipt)
    if record.get('repository') != str(root) or record.get('mode') != 'live-reviewed-proposal-only':
        msg = 'Receipt repository or mode mismatch'
        raise WorkflowError(msg)
    if record.get('verdict') != 'accept' or record.get('applied') is not False:
        msg = 'Receipt was not an accepted proposal'
        raise WorkflowError(msg)
    baseline = record.get('head')
    if not isinstance(baseline, str) or re.fullmatch(r'[0-9a-f]{40}', baseline) is None:
        msg = 'Invalid receipt baseline'
        raise WorkflowError(msg)
    task_id, owned, proposal = _admitted_proposal(record)
    path = workspace.resolve(strict=True)
    if path == root or path.is_relative_to(root) or root.is_relative_to(path):
        msg = 'Worktree must be disjoint from source'
        raise WorkflowError(msg)
    if _git(path, 'rev-parse', '--show-toplevel').strip() != str(path):
        msg = 'Expected a Git worktree root'
        raise WorkflowError(msg)
    branch = _git(path, 'symbolic-ref', '--short', 'HEAD').strip()
    if branch != f'harness/{task_id}':
        msg = 'Unexpected worktree branch'
        raise WorkflowError(msg)
    if _git(root, 'rev-parse', 'HEAD').strip() != baseline:
        msg = 'Source baseline changed'
        raise WorkflowError(msg)
    try:
        audit = audit_worktree(path, baseline, owned)
    except WorktreeError as exc:
        raise WorkflowError(str(exc)) from exc
    if audit.head != baseline or tuple(sorted(owned)) != audit.changed_paths:
        msg = 'Worktree changed paths or HEAD differ from reviewed receipt'
        raise WorkflowError(msg)
    verified_files = []
    for item in proposal.files:
        target = path / item.path
        if target.is_symlink() or not target.is_file():
            msg = 'Proposed file missing or symlinked'
            raise WorkflowError(msg)
        actual = target.read_bytes()
        expected = item.content.encode('utf-8')
        if actual != expected:
            msg = f'Worktree content mismatch: {item.path}'
            raise WorkflowError(msg)
        verified_files.append({'path': item.path, 'sha256': hashlib.sha256(actual).hexdigest()})
    if evidence.is_symlink() or evidence.exists():
        msg = 'Verification evidence path already exists'
        raise WorkflowError(msg)
    if evidence.parent.is_symlink() or not evidence.parent.is_dir():
        msg = 'Evidence parent must be an existing real directory'
        raise WorkflowError(msg)
    result: dict[str, object] = {
        'mode': 'read-only-integrity-verification',
        'task_id': task_id,
        'repository': str(root),
        'workspace': str(path),
        'baseline': baseline,
        'branch': branch,
        'changed_paths': list(audit.changed_paths),
        'files': verified_files,
        'integrity_verified': True,
        'tests_executed': False,
        'generated_code_executed': False,
        'committed': False,
        'merged': False,
        'human_decision_pending': True,
    }
    payload = (json.dumps(result, sort_keys=True, indent=2) + '\n').encode('utf-8')
    if len(payload) > _MAX_EVIDENCE_BYTES:
        msg = 'Evidence payload too large'
        raise WorkflowError(msg)
    descriptor = os.open(evidence, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, 'wb') as stream:
        stream.write(payload)
    return {
        **result,
        'evidence': str(evidence),
        'evidence_sha256': hashlib.sha256(payload).hexdigest(),
    }
