"""Opt-in Hermes skill transaction: real local inference, immutable review receipt.

No tool execution, filesystem application, Git mutations, or merge authority.
"""

import difflib
import json
import os
import re
from typing import TYPE_CHECKING

from hermes_harness.multiagent.local_inference import LocalEndpoint
from hermes_harness.multiagent.model_pipeline import generate_and_review
from hermes_harness.multiagent.ollama_native import (
    PROPOSAL_SCHEMA,
    REVIEW_SCHEMA,
    NativeOllamaJSONTransport,
)
from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.workflows import repository

if TYPE_CHECKING:
    from pathlib import Path

MAX_SESSION_ID_LENGTH = 128

_ID = re.compile(r'[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}\Z')
_MAX_OBJECTIVE = 4096
_MAX_DIFF = 64_000


def reviewed_live_task(  # noqa: C901, PLR0913, PLR0914, PLR0915 - bounded native inference and evidence boundary
    *,
    repo: Path,
    state: Path,
    session: str,
    task_id: str,
    objective: str,
    owned_path: str,
    authorized: bool = False,
) -> dict[str, object]:
    """Request one new-file proposal and independent review, then save evidence."""
    if not authorized:
        msg = 'Explicit inference authorization required'
        raise WorkflowError(msg)
    if not _ID.fullmatch(task_id) or not session.strip() or len(session) > MAX_SESSION_ID_LENGTH:
        msg = 'Invalid task or session identity'
        raise WorkflowError(msg)
    if not 1 <= len(objective) <= _MAX_OBJECTIVE:
        msg = 'Invalid objective length'
        raise WorkflowError(msg)
    if state.is_symlink() or not state.is_dir():
        msg = 'State must be an existing real directory'
        raise WorkflowError(msg)
    root = repository(repo)
    if (root / owned_path).exists() or (root / owned_path).is_symlink():
        msg = 'Live new-file mode refuses existing destination'
        raise WorkflowError(msg)
    receipt = state / f'{task_id}.json'
    if receipt.exists() or receipt.is_symlink():
        msg = 'Task receipt already exists; refusing redispatch'
        raise WorkflowError(msg)
    url = 'http://127.0.0.1:11434/v1/chat/completions'
    worker = LocalEndpoint(url, 'qwen2.5-coder:7b', timeout_seconds=120, max_tokens=2048)
    reviewer = LocalEndpoint(url, 'qwen3:4b', timeout_seconds=120, max_tokens=512)
    transport = NativeOllamaJSONTransport(
        schemas_by_model={
            worker.model: PROPOSAL_SCHEMA,
            reviewer.model: REVIEW_SCHEMA,
        }
    )
    result = generate_and_review(
        task_id=task_id,
        objective=objective,
        owned_paths=(owned_path,),
        worker=worker,
        reviewer=reviewer,
        transport=transport,
        inference_authorized=True,
    )
    files: list[dict[str, str]] = []
    for item in result.proposal.files:
        diff = ''.join(
            difflib.unified_diff(
                [],
                item.content.splitlines(keepends=True),
                fromfile='/dev/null',
                tofile=f'b/{item.path}',
            )
        )
        if len(diff) > _MAX_DIFF:
            msg = 'Proposal diff exceeds inspection limit'
            raise WorkflowError(msg)
        files.append({'path': item.path, 'sha256': item.sha256, 'diff': diff})
    evidence: dict[str, object] = {
        'action': 'agents-assemble',
        'mode': 'live-reviewed-proposal-only',
        'session_id': session,
        'task_id': task_id,
        'repository': str(root),
        'head': _head(root),
        'worker_model': worker.model,
        'reviewer_model': reviewer.model,
        'verdict': result.review.verdict,
        'findings': list(result.review.findings),
        'files': files,
        'applied': False,
        'executed_generated_code': False,
        'merged': False,
        'human_decision_pending': True,
    }
    fd = os.open(receipt, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w', encoding='utf-8') as stream:
        json.dump(evidence, stream, sort_keys=True, indent=2)
        stream.write('\n')
    evidence['receipt'] = str(receipt)
    return evidence


def _head(root: Path) -> str:
    import subprocess  # noqa: PLC0415 - narrow Git inspection boundary

    result = subprocess.run(  # noqa: S603
        ['/usr/bin/git', '-C', str(root), 'rev-parse', 'HEAD'],
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
    )
    if result.returncode:
        msg = 'Cannot resolve repository HEAD'
        raise WorkflowError(msg)
    return result.stdout.strip()
