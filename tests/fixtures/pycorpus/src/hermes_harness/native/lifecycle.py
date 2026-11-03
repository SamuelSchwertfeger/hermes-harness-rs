"""Append-only checkpoint connecting reviewed proposals to verified worktrees.

This is a bookkeeping gate, not permission to run generated code or repair it.
"""

import hashlib
import json
import os
import re
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from typing import Any

_MAX_BYTES = 128 * 1024
_HEAD = re.compile(r'[0-9a-f]{40}\Z')


def _load(path: Path) -> tuple[dict[str, Any], str]:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > _MAX_BYTES:
        msg = 'Evidence must be a regular, bounded, non-symlink file'
        raise WorkflowError(msg)
    payload = path.read_bytes()
    try:
        record = json.loads(payload)
    except (ValueError, UnicodeDecodeError) as exc:
        msg = 'Evidence is not valid JSON'
        raise WorkflowError(msg) from exc
    if not isinstance(record, dict):
        msg = 'Evidence must be a JSON object'
        raise WorkflowError(msg)
    return record, hashlib.sha256(payload).hexdigest()


_MAX_CHECKPOINT_FILES = 16


def _files(record: dict[str, Any]) -> dict[str, str]:
    rows = record.get('files')
    if not isinstance(rows, list) or not rows or len(rows) > _MAX_CHECKPOINT_FILES:
        msg = 'Evidence file list is invalid'
        raise WorkflowError(msg)
    result: dict[str, str] = {}
    for row in rows:
        if not isinstance(row, dict):
            msg = 'Invalid evidence file entry'
            raise WorkflowError(msg)
        name, digest = row.get('path'), row.get('sha256')
        if not isinstance(name, str) or not isinstance(digest, str):
            msg = 'Invalid evidence path or digest'
            raise WorkflowError(msg)
        if not name or name.startswith('/') or '..' in Path(name).parts or '\\' in name:  # noqa: PLR0916
            msg = 'Invalid owned path'
            raise WorkflowError(msg)
        if not re.fullmatch(r'[0-9a-f]{64}', digest) or name in result:
            msg = 'Invalid or repeated file digest'
            raise WorkflowError(msg)
        result[name] = digest
    return result


def record_checkpoint(  # noqa: C901, PLR0912, PLR0913, PLR0914, PLR0915 - evidence boundary
    *, receipt: Path, verification: Path, evidence: Path, authorized: bool = False
) -> dict[str, object]:
    """Link accepted review and passed integrity results; do not execute code."""
    if not authorized:
        msg = 'Explicit authorization to record a checkpoint is required'
        raise WorkflowError(msg)
    review, review_hash = _load(receipt)
    verified, verification_hash = _load(verification)
    baseline = review.get('head')
    if not isinstance(baseline, str) or not _HEAD.fullmatch(baseline):
        msg = 'Review baseline invalid'
        raise WorkflowError(msg)
    if (
        review.get('mode') != 'live-reviewed-proposal-only'  # noqa: PLR0916
        or review.get('verdict') != 'accept'
        or review.get('applied') is not False
        or review.get('human_decision_pending') is not True
    ):
        msg = 'Review receipt is not an accepted, pending proposal'
        raise WorkflowError(msg)
    if (
        verified.get('mode') != 'read-only-integrity-verification'  # noqa: PLR0916
        or verified.get('integrity_verified') is not True
        or verified.get('tests_executed') is not False
        or verified.get('generated_code_executed') is not False
        or verified.get('human_decision_pending') is not True
    ):
        msg = 'Verification receipt is not a valid integrity-only result'
        raise WorkflowError(msg)
    for review_key, verified_key in (
        ('task_id', 'task_id'),
        ('repository', 'repository'),
        ('head', 'baseline'),
    ):
        if review.get(review_key) != verified.get(verified_key):
            msg = f'Review and verification mismatch: {review_key}'
            raise WorkflowError(msg)
    original_files = _files(review)
    verified_files = _files(verified)
    if original_files != verified_files or sorted(verified_files) != sorted(
        verified.get('changed_paths', [])
    ):
        msg = 'Reviewed and verified paths or digests differ'
        raise WorkflowError(msg)
    workspace, branch, task_id = (
        verified.get('workspace'),
        verified.get('branch'),
        review.get('task_id'),
    )
    if not isinstance(task_id, str) or not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', task_id):
        msg = 'Invalid task identifier'
        raise WorkflowError(msg)
    if (
        not isinstance(workspace, str)
        or not workspace.startswith('/')
        or branch != f'harness/{task_id}'
    ):
        msg = 'Verification worktree identity invalid'
        raise WorkflowError(msg)
    result: dict[str, object] = {
        'mode': 'integrity-checkpoint',
        'task_id': task_id,
        'repository': review['repository'],
        'baseline': baseline,
        'workspace': workspace,
        'branch': branch,
        'review_receipt_sha256': review_hash,
        'verification_receipt_sha256': verification_hash,
        'files': [
            {'path': name, 'sha256': original_files[name]} for name in sorted(original_files)
        ],
        'integrity_verified': True,
        'functional_verified': False,
        'tests_executed': False,
        'generated_code_executed': False,
        'repair_attempts': 0,
        'max_repair_attempts': 3,
        'next_stage': 'requires-separate-functional-execution-authorization',
        'committed': False,
        'merged': False,
        'human_decision_pending': True,
    }
    payload = (json.dumps(result, indent=2, sort_keys=True) + '\n').encode()
    if len(payload) > _MAX_BYTES:
        msg = 'Checkpoint evidence too large'
        raise WorkflowError(msg)
    if (
        evidence.is_symlink()  # noqa: PLR0916
        or evidence.exists()
        or evidence.parent.is_symlink()
        or not evidence.parent.is_dir()
    ):
        msg = 'Checkpoint evidence destination must be unused within a real directory'
        raise WorkflowError(msg)
    fd = os.open(evidence, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(payload)
    return {
        **result,
        'evidence': str(evidence),
        'evidence_sha256': hashlib.sha256(payload).hexdigest(),
    }
