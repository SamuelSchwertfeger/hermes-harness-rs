"""Durably request human review from authenticated receipts and Git audit.

Never approves, merges, dispatches, or replays a gateway job.
"""

import hashlib
import json
import os
from dataclasses import asdict, dataclass
from typing import TYPE_CHECKING

from hermes_harness.agents.supervised_review_handoff import verify_reviewer_handoff
from hermes_harness.supervised.live_reviewer_verdict import VerdictBinding, parse_reviewer_verdict

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.agents.supervised_review_handoff import ReviewerHandoff
    from hermes_harness.gateway.gateway_terminal_reconciliation import TerminalEvidence
    from hermes_harness.git.worktree_delegation import TaskWorkspace
    from hermes_harness.supervised.live_reviewer_verdict import ReviewerVerdict


class HumanDecisionError(RuntimeError):
    """Evidence or durable decision request is unsafe or inconsistent."""


@dataclass(frozen=True, slots=True, kw_only=True)
class DecisionEvidence:
    """Existing verified terminal receipts, never synthetic review correctness."""

    worker: TerminalEvidence
    reviewer: TerminalEvidence


@dataclass(frozen=True, slots=True, kw_only=True)
class DecisionRequest:
    """Input bundle for a one-shot, human-only approval request."""

    task: TaskWorkspace
    handoff: ReviewerHandoff
    receipts: DecisionEvidence
    output_dir: Path


def _require_terminals(handoff: ReviewerHandoff, receipts: DecisionEvidence) -> str:
    worker = receipts.worker
    reviewer = receipts.reviewer
    if worker.submission_id != handoff.worker_submission_id:
        msg = 'Worker receipt does not match handoff'
        raise HumanDecisionError(msg)
    if worker.admission_id != handoff.worker_admission_id:
        msg = 'Worker admission does not match handoff'
        raise HumanDecisionError(msg)
    if worker.outcome != 'completed' or reviewer.outcome != 'completed':
        msg = 'Both gateway jobs require successful terminal outcomes'
        raise HumanDecisionError(msg)
    if not worker.final_response or not reviewer.final_response:
        msg = 'Both roles require nonempty terminal responses'
        raise HumanDecisionError(msg)
    if reviewer.submission_id == worker.submission_id:
        msg = 'Reviewer and worker submission identities must differ'
        raise HumanDecisionError(msg)
    if reviewer.admission_id == worker.admission_id:
        msg = 'Reviewer and worker admission identities must differ'
        raise HumanDecisionError(msg)
    return reviewer.final_response


def _request_document(
    handoff: ReviewerHandoff, receipts: DecisionEvidence, verdict: ReviewerVerdict
) -> dict[str, object]:
    return {
        'schema_version': 1,
        'task_id': handoff.task_id,
        'worker_submission_id': handoff.worker_submission_id,
        'worker_admission_id': handoff.worker_admission_id,
        'reviewer_submission_id': receipts.reviewer.submission_id,
        'reviewer_admission_id': receipts.reviewer.admission_id,
        'audit_sha256': handoff.audit_sha256,
        'reviewer_verdict': asdict(verdict),
        'worker_response_sha256': hashlib.sha256(
            (receipts.worker.final_response or '').encode()
        ).hexdigest(),
        'reviewer_response_sha256': hashlib.sha256(
            (receipts.reviewer.final_response or '').encode()
        ).hexdigest(),
        'approved': False,
        'human_decision': 'pending',
    }


def request_human_decision_once(request: DecisionRequest) -> Path:
    """Verify immutable handoff, validate reviewer JSON and write once atomically.

    Caller owns output directory; an existing file is never overwritten or retried.
    """
    handoff = request.handoff
    receipts = request.receipts
    output_dir = request.output_dir
    verify_reviewer_handoff(request.task, handoff)
    response = _require_terminals(handoff, receipts)
    verdict = parse_reviewer_verdict(
        response,
        VerdictBinding(
            task_id=handoff.task_id,
            worker_submission_id=handoff.worker_submission_id,
            audit_sha256=handoff.audit_sha256,
        ),
    )
    if output_dir.is_symlink() or not output_dir.is_dir():
        msg = 'Decision output directory must exist and cannot be a symlink'
        raise HumanDecisionError(msg)
    path = output_dir / 'human-decision-request.json'
    data = (
        json.dumps(
            _request_document(handoff, receipts, verdict), sort_keys=True, separators=(',', ':')
        ).encode()
        + b'\n'
    )
    flags = os.O_CREAT | os.O_EXCL | os.O_WRONLY
    if hasattr(os, 'O_NOFOLLOW'):
        flags |= os.O_NOFOLLOW
    try:
        fd = os.open(path, flags, 0o600)
    except FileExistsError as exc:
        msg = 'Decision evidence already exists; do not overwrite'
        raise HumanDecisionError(msg) from exc
    with os.fdopen(fd, 'wb') as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    return path
