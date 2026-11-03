"""Read-only, bounded evidence bundle for independent model review.

Reviewer input comes from persisted task receipts, not arbitrary shell access.
This module does not claim OS confinement or authorize a merge.
"""

import hashlib
import json
from typing import TYPE_CHECKING

from hermes_harness.multiagent.runtime import WorkflowError, inspect

if TYPE_CHECKING:
    from pathlib import Path

MAX_RECEIPT_BYTES = 16 * 1024
MAX_REVIEW_NODES = 32


def _receipt(path: Path) -> tuple[dict[str, object], str]:
    if path.is_symlink() or not path.is_file():
        msg = 'Review receipt missing or symlinked'
        raise WorkflowError(msg)
    if path.stat().st_size > MAX_RECEIPT_BYTES:
        msg = 'Review receipt oversized'
        raise WorkflowError(msg)
    data = path.read_bytes()
    value: object = json.loads(data)
    if not isinstance(value, dict):
        msg = 'Review receipt is not an object'
        raise WorkflowError(msg)
    return value, hashlib.sha256(data).hexdigest()


def review_bundle(state_root: Path, run_id: str) -> dict[str, object]:
    """Extract immutable receipt digests and decisions without redispatch."""
    summary = inspect(state_root, run_id)
    tasks = summary.get('tasks')
    if not isinstance(tasks, list) or len(tasks) > MAX_REVIEW_NODES:
        msg = 'Invalid number of reviewed tasks'
        raise WorkflowError(msg)
    directory = state_root / run_id
    if directory.is_symlink():
        msg = 'Symlink run directory'
        raise WorkflowError(msg)
    receipts: list[dict[str, object]] = []
    for task in tasks:
        if not isinstance(task, dict) or not isinstance(task.get('node'), str):
            msg = 'Malformed task reference'
            raise WorkflowError(msg)
        node_id = task['node']
        if task.get('state') != 'completed':
            msg = 'Review requires fully completed task evidence'
            raise WorkflowError(msg)
        record, digest = _receipt(directory / f'{node_id}.receipt.json')
        if record.get('state') != 'awaiting_approval':
            msg = 'Task receipt is not approval-ready'
            raise WorkflowError(msg)
        receipts.append({
            'node': node_id,
            'task_id': record.get('task_id'),
            'worker_digest': record.get('worker_sha256'),
            'review_digest': record.get('review_sha256'),
            'receipt_sha256': digest,
        })
    return {
        'run_id': run_id,
        'state': summary['state'],
        'receipt_evidence': receipts,
        'authorizes_merge': False,
    }
