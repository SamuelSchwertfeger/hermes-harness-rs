"""Explicitly gated, one-shot reviewer prompt submission after audited handoff.

No gateway is created or connected here. A caller must inject an already qualified
runtime and an authenticated RPC. The default gate never authorizes execution.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

from hermes_harness.agents.supervised_review_handoff import verify_reviewer_handoff
from hermes_harness.gateway.gateway_live_admission import LiveAdmissionRequest, submit_once

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.agents.supervised_review_handoff import ReviewerHandoff
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.gateway.gateway_identity import Submission
    from hermes_harness.gateway.gateway_live_admission import AsyncGatewayRPC, LiveAdmissionResult
    from hermes_harness.git.worktree_delegation import TaskWorkspace


class ReviewerDispatchBlockedError(RuntimeError):
    """The reviewer may not be admitted to a gateway."""


@dataclass(frozen=True, slots=True, kw_only=True)
class RuntimeBinding:
    """Exact task, reviewer, session and resolved worktree requiring attestation."""

    task_id: str
    reviewer_id: str
    session_id: str
    workspace: Path
    audit_sha256: str


class RuntimeQualifier(Protocol):
    """External, independently qualified execution and isolation gate."""

    def require_qualified(self, binding: RuntimeBinding) -> None:
        """Raise unless sandbox, gateway identity, cancellation and scope are proven."""
        ...


class DisabledRuntimeQualifier:
    """Safe default; no live runtime is implicitly trusted."""

    def require_qualified(self, _binding: RuntimeBinding) -> None:
        msg = 'Gateway execution disabled until runtime isolation and cancellation are qualified'
        raise ReviewerDispatchBlockedError(msg)


@dataclass(frozen=True, slots=True, kw_only=True)
class ReviewerDispatchInputs:
    task: TaskWorkspace
    handoff: ReviewerHandoff
    submission: Submission
    store: AdmissionStore
    rpc: AsyncGatewayRPC
    qualifier: RuntimeQualifier


def _check_binding(inputs: ReviewerDispatchInputs) -> RuntimeBinding:
    task = inputs.task
    handoff = inputs.handoff
    submission = inputs.submission
    if not submission.session_id.strip() or not submission.submission_id.strip():
        msg = 'Reviewer session and submission identities are required'
        raise ReviewerDispatchBlockedError(msg)
    if submission.admission_id is not None:
        msg = 'Reviewer submission is already admitted; do not resubmit'
        raise ReviewerDispatchBlockedError(msg)
    if inputs.store.get(submission.submission_id) is not None:
        msg = 'Reviewer submission already recorded or uncertain; do not resubmit'
        raise ReviewerDispatchBlockedError(msg)
    if task.workspace.is_symlink() or not task.workspace.is_dir():
        msg = 'Reviewer workspace must be an existing real directory'
        raise ReviewerDispatchBlockedError(msg)
    verify_reviewer_handoff(task, handoff)
    return RuntimeBinding(
        task_id=handoff.task_id,
        reviewer_id=handoff.reviewer_id,
        session_id=submission.session_id,
        workspace=task.workspace.resolve(strict=True),
        audit_sha256=handoff.audit_sha256,
    )


async def submit_reviewer_once(inputs: ReviewerDispatchInputs) -> LiveAdmissionResult:
    """Submit exactly once after independent runtime attestation and audit recheck.

    The admission store fences the submission before the RPC. A transport error,
    malformed response, or uncertainty must be reconciled, never retried.
    """
    binding = _check_binding(inputs)
    inputs.qualifier.require_qualified(binding)
    # Reverify immediately before the irreversible dispatch (best-effort TOCTOU guard).
    verify_reviewer_handoff(inputs.task, inputs.handoff)
    return await submit_once(
        inputs.store,
        LiveAdmissionRequest(inputs.submission, inputs.handoff.reviewer_prompt),
        rpc=inputs.rpc,
    )


# Temporary exports for legacy module import compatibility.
__all__ = [
    'DisabledRuntimeQualifier',
    'LiveAdmissionRequest',
    'Protocol',
    'ReviewerDispatchBlockedError',
    'ReviewerDispatchInputs',
    'RuntimeBinding',
    'RuntimeQualifier',
    '_check_binding',
    'dataclass',
    'submit_once',
    'submit_reviewer_once',
    'verify_reviewer_handoff',
]
