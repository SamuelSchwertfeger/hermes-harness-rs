"""One-shot offline worker/reviewer coordinator with durable fail-closed fences.

Only caller-supplied, controlled offline agents are supported. No live runtime,
provider, network, merge, approval grant, or automatic recovery is performed.
"""

from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING

from hermes_harness.core.delegation import DelegationError, Phase, phase
from hermes_harness.core.task_reservation import RecoveryState, reconcile
from hermes_harness.git.worktree_delegation import (
    delegate_in_worktree,
    prepare_worktree,
    request_worktree_approval,
    review_worktree,
    verified_audit,
)

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.core.delegation import Finding, OfflineAgent
    from hermes_harness.git.worktree_delegation import TaskWorkspace


@dataclass(frozen=True, slots=True)
class OfflineAgents:
    """Two deliberately supplied offline agent implementations."""

    worker: OfflineAgent
    reviewer: OfflineAgent


class OrchestrationState(StrEnum):
    """Result of fresh orchestration or read-only inspection."""

    AWAITING_APPROVAL = 'awaiting_approval'
    RESERVED = 'reserved'
    PREPARING = 'preparing_uncertain'
    PREPARED = 'prepared'
    DISPATCH_UNCERTAIN = 'dispatch_uncertain'
    WORKER_RECORDED = 'worker_recorded'
    REVIEW_UNCERTAIN = 'review_uncertain'
    REVIEWED = 'reviewed'
    MANUAL_INTERVENTION = 'manual_intervention'


@dataclass(frozen=True, slots=True)
class OrchestrationResult:
    """Non-authorizing task status and local evidence references."""

    state: OrchestrationState
    task_id: str
    worker: Finding | None = None
    reviewer: Finding | None = None
    approval_request: Path | None = None


def run_offline_task(
    task: TaskWorkspace, source_repo: Path, agents: OfflineAgents
) -> OrchestrationResult:
    """Run exactly one fresh offline task; errors are terminal, never retried.

    Any exception leaves existing task evidence/worktree intact for manual inspection.
    Reinvoking this function for a reserved task fails at reservation creation.
    """
    if task.brief.worker_id == task.brief.reviewer_id or agents.worker is agents.reviewer:
        msg = 'Worker and reviewer must have distinct identities and implementations'
        raise DelegationError(msg)
    prepare_worktree(task, source_repo)
    worker = delegate_in_worktree(task, agents.worker)
    verified_audit(task)
    reviewer = review_worktree(task, agents.reviewer)
    approval = request_worktree_approval(task)
    return OrchestrationResult(
        OrchestrationState.AWAITING_APPROVAL,
        task.brief.task_id,
        worker,
        reviewer,
        approval,
    )


def inspect_offline_task(task: TaskWorkspace) -> OrchestrationResult:
    """Inspect durable records without dispatch, mutation, or implicit recovery."""
    recovery = reconcile(task)
    if recovery is not RecoveryState.WORKER_RECORDED:
        return OrchestrationResult(OrchestrationState(recovery.value), task.brief.task_id)
    directory = task.state_root / task.brief.task_id
    if (directory / 'review_dispatch.json').is_symlink():
        return OrchestrationResult(OrchestrationState.MANUAL_INTERVENTION, task.brief.task_id)
    review_fenced = (directory / 'review_dispatch.json').exists()
    try:
        current_phase = phase(task.state_root, task.brief)
    except DelegationError, OSError, ValueError, TypeError:
        return OrchestrationResult(OrchestrationState.MANUAL_INTERVENTION, task.brief.task_id)
    if current_phase is Phase.AWAITING_APPROVAL:
        return OrchestrationResult(
            OrchestrationState.AWAITING_APPROVAL,
            task.brief.task_id,
            approval_request=directory / 'approval-request.json',
        )
    if current_phase is Phase.REVIEWED:
        return OrchestrationResult(OrchestrationState.REVIEWED, task.brief.task_id)
    state = (
        OrchestrationState.REVIEW_UNCERTAIN if review_fenced else OrchestrationState.WORKER_RECORDED
    )
    return OrchestrationResult(state, task.brief.task_id)
