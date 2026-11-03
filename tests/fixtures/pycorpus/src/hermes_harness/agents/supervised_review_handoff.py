"""Bind a verified worker gateway completion to an audited reviewer prompt.

Read-only with respect to the gateway: no prompt submission, session creation,
retry, approval, or agent execution is performed here.
"""

import hashlib
import json
from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.gateway.gateway_terminal_reconciliation import reconcile_terminal_once
from hermes_harness.git.worktree_delegation import verified_audit

if TYPE_CHECKING:
    from hermes_harness.agents.supervised_agents import SupervisedPlan
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.gateway.gateway_terminal_reconciliation import ReceiptReader
    from hermes_harness.gateway.supervised_gateway_receipts import RoleAdmissions
    from hermes_harness.git.worktree_delegation import TaskWorkspace

MAX_REVIEW_PROMPT_CHARS = 8_000


class ReviewerHandoffError(RuntimeError):
    """Unbound, stale, or unsuccessful worker evidence."""


@dataclass(frozen=True, slots=True, kw_only=True)
class ReviewerHandoff:
    """Read-only proposal; caller must separately authorize reviewer submission."""

    task_id: str
    reviewer_id: str
    worker_submission_id: str
    worker_admission_id: str
    audit_sha256: str
    reviewer_prompt: str


def _audit_digest(task: TaskWorkspace) -> str:
    verified_audit(task)
    path = task.state_root / task.brief.task_id / 'worktree-audit.json'
    if path.is_symlink() or not path.is_file():
        msg = 'Worktree audit record is missing or a symlink'
        raise ReviewerHandoffError(msg)
    # verified_audit checked this record against Git and current file contents.
    document: object = json.loads(path.read_bytes())
    if not isinstance(document, dict):
        msg = 'Invalid worktree audit record'
        raise ReviewerHandoffError(msg)
    canonical = json.dumps(document, sort_keys=True, separators=(',', ':')).encode()
    return hashlib.sha256(canonical).hexdigest()


@dataclass(frozen=True, slots=True, kw_only=True)
class HandoffSources:
    """Read-only receipt inputs bound to the planned role admissions."""

    ids: RoleAdmissions
    store: AdmissionStore
    rpc: ReceiptReader


def _validate_handoff_inputs(
    plan: SupervisedPlan, task: TaskWorkspace, sources: HandoffSources
) -> str:
    ids = sources.ids
    if plan.task_id != task.brief.task_id or ids.task_id != plan.task_id:
        msg = 'Handoff task identity mismatch'
        raise ReviewerHandoffError(msg)
    if plan.worker_id != task.brief.worker_id or plan.reviewer_id != task.brief.reviewer_id:
        msg = 'Handoff role identity mismatch'
        raise ReviewerHandoffError(msg)
    if not ids.worker_submission_id or not ids.reviewer_submission_id:
        msg = 'Both role submission identities are required'
        raise ReviewerHandoffError(msg)
    if ids.worker_submission_id == ids.reviewer_submission_id:
        msg = 'Reviewer must have distinct submission identity'
        raise ReviewerHandoffError(msg)
    if sources.store.get(ids.reviewer_submission_id) is not None:
        msg = 'Reviewer already submitted or uncertain; do not dispatch again'
        raise ReviewerHandoffError(msg)
    worker = sources.store.get(ids.worker_submission_id)
    if worker is None or not worker.admission_id:
        msg = 'Worker requires a durable admission before review'
        raise ReviewerHandoffError(msg)
    return ids.worker_submission_id


@dataclass(frozen=True, slots=True)
class _WorkerBinding:
    submission_id: str
    admission_id: str
    digest: str


def _reviewer_prompt(plan: SupervisedPlan, binding: _WorkerBinding) -> str:
    prompt = (
        f'{plan.reviewer.prompt}\n\n'
        'Immutable worker-to-reviewer evidence binding:\n'
        f'Task: {plan.task_id}\n'
        f'Worker submission: {binding.submission_id}\n'
        f'Worker admission: {binding.admission_id}\n'
        f'Worker worktree audit SHA256: {binding.digest}\n'
        'Independently inspect the worktree and compare its current audit to the '
        'bound digest. Do not write, push, merge, or grant approval.'
    )
    if len(prompt) > MAX_REVIEW_PROMPT_CHARS:
        msg = 'Reviewer prompt exceeds bounded request size'
        raise ReviewerHandoffError(msg)
    return prompt


async def prepare_reviewer_handoff_once(
    plan: SupervisedPlan, task: TaskWorkspace, sources: HandoffSources
) -> ReviewerHandoff:
    """Read the worker's known receipt and bind reviewer input to a fresh Git audit.

    May be called after interruption for observation, but never resubmits a
    worker or reviewer. Reviewer dispatch must use the existing one-shot ledger.
    """
    worker_submission_id = _validate_handoff_inputs(plan, task, sources)
    evidence = await reconcile_terminal_once(sources.rpc, sources.store, worker_submission_id)
    if evidence.outcome != 'completed' or not evidence.final_response:
        msg = 'Worker requires successful nonempty terminal evidence'
        raise ReviewerHandoffError(msg)
    digest = _audit_digest(task)
    binding = _WorkerBinding(worker_submission_id, evidence.admission_id, digest)
    return ReviewerHandoff(
        task_id=plan.task_id,
        reviewer_id=plan.reviewer_id,
        worker_submission_id=worker_submission_id,
        worker_admission_id=evidence.admission_id,
        audit_sha256=digest,
        reviewer_prompt=_reviewer_prompt(plan, binding),
    )


def verify_reviewer_handoff(task: TaskWorkspace, handoff: ReviewerHandoff) -> None:
    """Fail closed if evidence changed since the reviewer input was prepared."""
    if task.brief.task_id != handoff.task_id or task.brief.reviewer_id != handoff.reviewer_id:
        msg = 'Reviewer handoff identity mismatch'
        raise ReviewerHandoffError(msg)
    if _audit_digest(task) != handoff.audit_sha256:
        msg = 'Reviewer handoff audit changed'
        raise ReviewerHandoffError(msg)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_REVIEW_PROMPT_CHARS',
    'HandoffSources',
    'ReviewerHandoff',
    'ReviewerHandoffError',
    '_WorkerBinding',
    '_audit_digest',
    '_reviewer_prompt',
    '_validate_handoff_inputs',
    'dataclass',
    'hashlib',
    'json',
    'prepare_reviewer_handoff_once',
    'reconcile_terminal_once',
    'verified_audit',
    'verify_reviewer_handoff',
]
