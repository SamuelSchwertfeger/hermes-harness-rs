"""Opt-in gateway-owned role admissions; no implicit live transport or simulation.

This is a staging API, not an end-to-end live executor. A caller must supply
an externally qualified sandbox, authenticated RPC and independently audited
worktree evidence before requesting reviewer admission.
"""

from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from hermes_harness.agents.supervised_review_handoff import (
    HandoffSources,
    prepare_reviewer_handoff_once,
    verify_reviewer_handoff,
)
from hermes_harness.agents.supervised_reviewer_dispatch import (
    DisabledRuntimeQualifier,
    ReviewerDispatchInputs,
    RuntimeBinding,
    submit_reviewer_once,
)
from hermes_harness.gateway.gateway_live_admission import LiveAdmissionRequest, submit_once
from hermes_harness.gateway.gateway_terminal_reconciliation import reconcile_terminal_once

if TYPE_CHECKING:
    from hermes_harness.agents.supervised_agents import SupervisedPlan
    from hermes_harness.agents.supervised_reviewer_dispatch import RuntimeQualifier
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.gateway.gateway_identity import Submission
    from hermes_harness.gateway.gateway_live_admission import AsyncGatewayRPC, LiveAdmissionResult
    from hermes_harness.gateway.gateway_terminal_reconciliation import TerminalEvidence
    from hermes_harness.gateway.supervised_gateway_receipts import RoleAdmissions
    from hermes_harness.git.worktree_delegation import TaskWorkspace


class LiveSupervisorBlockedError(RuntimeError):
    """A missing runtime attestation or identity prevents dispatch."""


@dataclass(frozen=True, slots=True, kw_only=True)
class LiveSupervisorInputs:
    """Explicit, externally supplied admission dependencies; no transport factory."""

    task: TaskWorkspace
    plan: SupervisedPlan
    ids: RoleAdmissions
    worker_submission: Submission
    reviewer_submission: Submission
    store: AdmissionStore
    rpc: AsyncGatewayRPC
    qualifier: RuntimeQualifier = field(default_factory=DisabledRuntimeQualifier)


def _validate(inputs: LiveSupervisorInputs) -> None:
    if inputs.plan.task_id != inputs.task.brief.task_id:
        msg = 'Plan task does not match worktree task'
        raise LiveSupervisorBlockedError(msg)
    if inputs.ids.task_id != inputs.plan.task_id:
        msg = 'Admission task does not match plan'
        raise LiveSupervisorBlockedError(msg)
    if inputs.ids.worker_submission_id != inputs.worker_submission.submission_id:
        msg = 'Worker submission ID mismatch'
        raise LiveSupervisorBlockedError(msg)
    if inputs.ids.reviewer_submission_id != inputs.reviewer_submission.submission_id:
        msg = 'Reviewer submission ID mismatch'
        raise LiveSupervisorBlockedError(msg)
    if inputs.worker_submission.session_id == inputs.reviewer_submission.session_id:
        msg = 'Separate gateway sessions required'
        raise LiveSupervisorBlockedError(msg)
    if inputs.worker_submission.submission_id == inputs.reviewer_submission.submission_id:
        msg = 'Separate gateway submissions required'
        raise LiveSupervisorBlockedError(msg)
    if inputs.task.workspace.is_symlink() or not inputs.task.workspace.is_dir():
        msg = 'Real prepared worktree required'
        raise LiveSupervisorBlockedError(msg)


async def submit_live_worker_once(inputs: LiveSupervisorInputs) -> LiveAdmissionResult:
    """Submit worker exactly once, with durable pre-RPC fencing and external qualification."""
    _validate(inputs)
    if inputs.store.get(inputs.worker_submission.submission_id) is not None:
        msg = 'Worker already recorded or uncertain; never resubmit'
        raise LiveSupervisorBlockedError(msg)
    inputs.qualifier.require_qualified(
        RuntimeBinding(
            task_id=inputs.plan.task_id,
            reviewer_id=inputs.plan.worker_id,
            session_id=inputs.worker_submission.session_id,
            workspace=inputs.task.workspace.resolve(strict=True),
            audit_sha256='pre-worker',
        )
    )
    return await submit_once(
        inputs.store,
        LiveAdmissionRequest(inputs.worker_submission, inputs.plan.worker.prompt),
        rpc=inputs.rpc,
    )


async def inspect_live_worker(inputs: LiveSupervisorInputs) -> TerminalEvidence:
    """Reconcile only a known worker admission; never create or resubmit work."""
    _validate(inputs)
    return await reconcile_terminal_once(
        inputs.rpc, inputs.store, inputs.worker_submission.submission_id
    )


async def submit_live_reviewer_once(inputs: LiveSupervisorInputs) -> LiveAdmissionResult:
    """Dispatch a distinct reviewer only after externally recorded Git audit.

    Existing worktree audit mechanisms must already have proven a real worker's
    filesystem changes. This method never fabricates that evidence.
    """
    _validate(inputs)
    handoff = await prepare_reviewer_handoff_once(
        inputs.plan,
        inputs.task,
        HandoffSources(ids=inputs.ids, store=inputs.store, rpc=inputs.rpc),
    )
    verify_reviewer_handoff(inputs.task, handoff)
    return await submit_reviewer_once(
        ReviewerDispatchInputs(
            task=inputs.task,
            handoff=handoff,
            submission=inputs.reviewer_submission,
            store=inputs.store,
            rpc=inputs.rpc,
            qualifier=inputs.qualifier,
        )
    )


async def inspect_live_reviewer(inputs: LiveSupervisorInputs) -> TerminalEvidence:
    """Reconcile a previously admitted reviewer without replay or approval."""
    _validate(inputs)
    return await reconcile_terminal_once(
        inputs.rpc, inputs.store, inputs.reviewer_submission.submission_id
    )


# Temporary exports for legacy module import compatibility.
__all__ = [
    'DisabledRuntimeQualifier',
    'HandoffSources',
    'LiveAdmissionRequest',
    'LiveSupervisorBlockedError',
    'LiveSupervisorInputs',
    'ReviewerDispatchInputs',
    'RuntimeBinding',
    '_validate',
    'dataclass',
    'field',
    'inspect_live_reviewer',
    'inspect_live_worker',
    'prepare_reviewer_handoff_once',
    'reconcile_terminal_once',
    'submit_live_reviewer_once',
    'submit_live_worker_once',
    'submit_once',
    'submit_reviewer_once',
    'verify_reviewer_handoff',
]
