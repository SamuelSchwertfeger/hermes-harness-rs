"""Receipt-only supervised finalization, with durable human-pending recovery.

This module cannot launch a service, submit a prompt, approve, merge or push.
It consumes preexisting worker/reviewer admissions and separately audited work.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.agents.supervised_review_handoff import verify_reviewer_handoff
from hermes_harness.gateway.supervised_gateway_receipts import (
    RoleAdmissions,
    reconcile_supervised_roles_once,
)
from hermes_harness.supervised.live_human_decision import (
    DecisionEvidence,
    DecisionRequest,
    request_human_decision_once,
)
from hermes_harness.supervised.live_supervised_recovery import (
    Phase,
    checkpoint_pending_human,
    checkpoint_reviewer_admission,
    checkpoint_worker_admission,
    checkpoint_worker_audit,
    inspect_pending_decision,
)

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.agents.supervised_agents import SupervisedPlan
    from hermes_harness.agents.supervised_review_handoff import ReviewerHandoff
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.gateway.supervised_gateway_receipts import (
        ReceiptSources,
    )
    from hermes_harness.git.worktree_delegation import TaskWorkspace
    from hermes_harness.supervised.live_supervised_recovery import (
        RecoveryCheckpoint,
        RecoveryIdentity,
        RecoveryLedger,
    )


class FinalizationBlockedError(RuntimeError):
    """Missing, conflicting or uncertain evidence prevents finalization."""


@dataclass(frozen=True, slots=True, kw_only=True)
class FinalizationInputs:
    """Operator-supplied existing evidence; never an execution instruction."""

    plan: SupervisedPlan
    task: TaskWorkspace
    handoff: ReviewerHandoff
    admissions: AdmissionStore
    receipts: ReceiptSources
    recovery: RecoveryLedger
    identity: RecoveryIdentity
    output_dir: Path


def _validate(inputs: FinalizationInputs) -> None:
    identity = inputs.identity
    if inputs.plan.task_id != identity.task_id or inputs.handoff.task_id != identity.task_id:
        msg = 'Task identity differs from recovered operation'
        raise FinalizationBlockedError(msg)
    if inputs.task.brief.task_id != identity.task_id:
        msg = 'Workspace differs from recovered operation'
        raise FinalizationBlockedError(msg)
    if inputs.handoff.worker_submission_id != identity.worker_submission_id:
        msg = 'Worker submission differs from recovered operation'
        raise FinalizationBlockedError(msg)
    if inputs.receipts.store is not inputs.admissions:
        msg = 'Receipt reconciliation must use authoritative admission store'
        raise FinalizationBlockedError(msg)
    if inputs.output_dir.is_symlink() or not inputs.output_dir.is_dir():
        msg = 'Decision output directory unavailable or unsafe'
        raise FinalizationBlockedError(msg)
    verify_reviewer_handoff(inputs.task, inputs.handoff)


def _advance_evidence(inputs: FinalizationInputs, phase: Phase) -> RecoveryCheckpoint:
    identity = inputs.identity
    if phase == Phase.PLANNED:
        return checkpoint_worker_admission(inputs.recovery, identity, inputs.admissions)
    if phase == Phase.WORKER_ADMITTED:
        return checkpoint_worker_audit(inputs.recovery, identity, (inputs.task, inputs.handoff))
    if phase == Phase.WORKER_AUDITED:
        return checkpoint_reviewer_admission(inputs.recovery, identity, inputs.admissions)
    msg = 'No additional offline evidence progression is available'
    raise FinalizationBlockedError(msg)


def _existing_decision(inputs: FinalizationInputs) -> RecoveryCheckpoint:
    """Crash recovery: verify existing artifact, never overwrite or recreate it."""
    path = inputs.output_dir / 'human-decision-request.json'
    inspect_pending_decision(path, inputs.identity)
    current = inputs.recovery.read(inputs.identity)
    if current.phase == Phase.PENDING_HUMAN:
        if current.evidence_sha256 != inspect_pending_decision(path, inputs.identity):
            msg = 'Pending artifact differs from persisted checkpoint'
            raise FinalizationBlockedError(msg)
        return current
    if current.phase != Phase.REVIEWER_ADMITTED:
        msg = 'Existing human request conflicts with recovery phase'
        raise FinalizationBlockedError(msg)
    return checkpoint_pending_human(inputs.recovery, inputs.identity, path)


async def finalize_existing_roles_once(inputs: FinalizationInputs) -> RecoveryCheckpoint:
    """Advance verified checkpoints and write at most one pending human request.

    An existing artifact is reconciled, never overwritten. No submission retry is
    possible: only prompt.receipt can be called, for already admitted identities.
    """
    _validate(inputs)
    checkpoint = inputs.recovery.initialize(inputs.identity)
    if checkpoint.phase == Phase.HALTED:
        msg = 'Operation halted; operator investigation required'
        raise FinalizationBlockedError(msg)
    decision_path = inputs.output_dir / 'human-decision-request.json'
    if decision_path.exists() or decision_path.is_symlink():
        return _existing_decision(inputs)
    if checkpoint.phase == Phase.PENDING_HUMAN:
        msg = 'Pending artifact missing; do not recreate after possible tampering'
        raise FinalizationBlockedError(msg)
    for phase in (Phase.PLANNED, Phase.WORKER_ADMITTED, Phase.WORKER_AUDITED):
        if checkpoint.phase == phase:
            checkpoint = _advance_evidence(inputs, phase)
    if checkpoint.phase != Phase.REVIEWER_ADMITTED:
        msg = 'Cannot finalize without independently admitted reviewer'
        raise FinalizationBlockedError(msg)
    completion = await reconcile_supervised_roles_once(
        inputs.plan,
        RoleAdmissions(
            task_id=inputs.identity.task_id,
            worker_submission_id=inputs.identity.worker_submission_id,
            reviewer_submission_id=inputs.identity.reviewer_submission_id,
        ),
        sources=inputs.receipts,
    )
    verify_reviewer_handoff(inputs.task, inputs.handoff)
    request_human_decision_once(
        DecisionRequest(
            task=inputs.task,
            handoff=inputs.handoff,
            receipts=DecisionEvidence(worker=completion.worker, reviewer=completion.reviewer),
            output_dir=inputs.output_dir,
        )
    )
    return checkpoint_pending_human(inputs.recovery, inputs.identity, decision_path)
