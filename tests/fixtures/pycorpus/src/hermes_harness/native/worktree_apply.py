"""Explicit human-granted application of a reviewed receipt to a disposable worktree.

Only creates new files. No generated-code execution, commits, pushes or merges.
"""

import hashlib
import json
import re
from typing import TYPE_CHECKING

from hermes_harness.git.worktrees import WorktreeSpec, create_worktree
from hermes_harness.multiagent.apply_edits import apply_new_files
from hermes_harness.multiagent.model_edits import parse_edit_proposal
from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.workflows import _git, repository

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.multiagent.model_edits import EditProposal

_MIN_DIFF_LINES = 4

_TASK_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,63}\Z')
_MAX_RECEIPT = 128 * 1024
_MAX_FILES = 16


def _read_receipt(path: Path) -> dict[str, object]:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > _MAX_RECEIPT:
        msg = 'Expected a bounded regular receipt file'
        raise WorkflowError(msg)
    data: object = json.loads(path.read_text(encoding='utf-8'))
    if not isinstance(data, dict):
        msg = 'Invalid receipt object'
        raise WorkflowError(msg)
    return data


def _content_from_new_file_diff(diff: str, path: str) -> str:
    """Decode only the new-file unified-diff form emitted by live_review."""
    lines = diff.splitlines(keepends=True)
    if (
        len(lines) < _MIN_DIFF_LINES
        or lines[0] != '--- /dev/null\n'
        or lines[1] != f'+++ b/{path}\n'
    ):
        msg = 'Invalid new-file diff headers'
        raise WorkflowError(msg)
    if not re.fullmatch(r'@@ -0,0 \+1,\d+ @@\n', lines[2]):
        msg = 'Invalid new-file diff hunk'
        raise WorkflowError(msg)
    if any(not line.startswith('+') for line in lines[3:]):
        msg = 'Invalid new-file diff lines'
        raise WorkflowError(msg)
    return ''.join(line[1:] for line in lines[3:])


def _admitted_proposal(record: dict[str, object]) -> tuple[str, tuple[str, ...], EditProposal]:
    task_id = record.get('task_id')
    rows = record.get('files')
    if not isinstance(task_id, str) or not _TASK_ID.fullmatch(task_id):
        msg = 'Invalid task identifier'
        raise WorkflowError(msg)
    if not isinstance(rows, list) or not 1 <= len(rows) <= _MAX_FILES:
        msg = 'Invalid file count in receipt'
        raise WorkflowError(msg)
    decoded = []
    for row in rows:
        if not isinstance(row, dict) or set(row) != {'path', 'sha256', 'diff'}:
            msg = 'Invalid file receipt schema'
            raise WorkflowError(msg)
        path, digest, diff = row['path'], row['sha256'], row['diff']
        if not all(isinstance(v, str) for v in (path, digest, diff)):
            msg = 'Invalid file receipt fields'
            raise WorkflowError(msg)
        content = _content_from_new_file_diff(diff, path)
        if hashlib.sha256(content.encode('utf-8')).hexdigest() != digest:
            msg = 'Receipt content hash mismatch'
            raise WorkflowError(msg)
        decoded.append({'path': path, 'content': content})
    owned = tuple(row['path'] for row in decoded)
    proposal = parse_edit_proposal(
        json.dumps({'task_id': task_id, 'files': decoded}), task_id, owned
    )
    return task_id, owned, proposal


def apply_reviewed_receipt(  # noqa: PLR0913, PLR0914 - explicit audited worktree boundary
    *, repo: Path, receipt: Path, workspace: Path, authorized: bool = False
) -> dict[str, object]:
    """Create and audit one disposable worktree after an explicit application grant."""
    if not authorized:
        msg = 'Explicit human authorization for worktree application is required'
        raise WorkflowError(msg)
    root = repository(repo)
    record = _read_receipt(receipt)
    expected = {
        'repository': str(root),
        'mode': 'live-reviewed-proposal-only',
        'verdict': 'accept',
        'applied': False,
        'human_decision_pending': True,
    }
    if any(
        type(record.get(key)) is not type(value) or record.get(key) != value
        for key, value in expected.items()
    ):
        msg = 'Receipt not eligible for worktree application'
        raise WorkflowError(msg)
    baseline = record.get('head')
    if not isinstance(baseline, str) or not re.fullmatch(r'[0-9a-f]{40}', baseline):
        msg = 'Invalid receipt baseline'
        raise WorkflowError(msg)
    if _git(root, 'rev-parse', 'HEAD').strip() != baseline:
        msg = 'Repository HEAD changed since review'
        raise WorkflowError(msg)
    task_id, owned, proposal = _admitted_proposal(record)
    spec = WorktreeSpec(task_id, f'harness/{task_id}', owned)
    # Existing helper refuses dirty sources, reused branches and existing workspaces.
    created = create_worktree(root, workspace, spec)
    if _git(created, 'rev-parse', 'HEAD').strip() != baseline:
        msg = 'Source HEAD changed since reviewed proposal; retain worktree for inspection'
        raise WorkflowError(msg)
    result = apply_new_files(created, proposal, owned, baseline, authorized=True)
    return {
        'mode': 'authorized-disposable-worktree',
        'task_id': task_id,
        'workspace': str(created),
        'baseline': baseline,
        'changed_paths': list(result.audit.changed_paths),
        'reviewer_verdict': 'accept',
        'generated_code_executed': False,
        'committed': False,
        'merged': False,
        'human_decision_pending': True,
    }
