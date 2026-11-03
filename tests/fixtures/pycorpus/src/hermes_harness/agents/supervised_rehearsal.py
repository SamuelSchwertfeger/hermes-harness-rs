"""Run the approved-workspace workflow with offline doubles only.

This is a rehearsal seam, NOT a Hermes agent adapter. It does not call a gateway,
launch a process, submit a prompt, or turn a disabled invocation executable.
"""

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.agents.supervised_agents import (
    MAX_SUPERVISED_DEADLINE_SECONDS,
    MAX_SUPERVISED_TURNS,
    prepare_supervised_plan,
)
from hermes_harness.core.delegation import DelegationError
from hermes_harness.git.worktree_delegation import (
    delegate_in_worktree,
    prepare_worktree,
    request_worktree_approval,
    review_worktree,
    verified_audit,
)
from hermes_harness.supervised.offline_orchestrator import OrchestrationResult, OrchestrationState

if TYPE_CHECKING:
    from hermes_harness.agents.supervised_agents import SupervisedPlan, SupervisedPolicy
    from hermes_harness.git.worktree_delegation import TaskWorkspace
    from hermes_harness.supervised.offline_orchestrator import OfflineAgents


@dataclass(frozen=True, slots=True)
class RehearsalResult:
    """Offline task outcome plus the explicitly non-executable role plan."""

    task: OrchestrationResult
    plan: SupervisedPlan


@dataclass(frozen=True, slots=True)
class RehearsalContext:
    """Source repository and authorization for one supervised rehearsal."""

    source_repo: Path
    policy: SupervisedPolicy


def run_supervised_rehearsal(
    task: TaskWorkspace,
    agents: OfflineAgents,
    context: RehearsalContext,
) -> RehearsalResult:
    """One-shot rehearsal; preserves evidence and never resumes uncertain work.

    Authorization is checked before reservation. Worktree preparation happens
    before preflight because preflight requires the worktree to exist. No dispatch
    occurs unless its exact workspace passes the regular supervised preflight.
    """
    policy = context.policy
    if policy.authorized_task_id != task.brief.task_id:
        msg = 'Policy does not authorize this task'
        raise PermissionError(msg)
    if policy.authorized_workspace != task.workspace or not task.workspace.is_absolute():
        msg = 'Policy must bind the exact absolute task workspace'
        raise PermissionError(msg)
    if agents.worker is agents.reviewer or task.brief.worker_id == task.brief.reviewer_id:
        msg = 'Rehearsal requires distinct worker and reviewer implementations'
        raise DelegationError(msg)
    # Fail before creating a reservation for invalid budgets.
    if (
        not 0 < policy.deadline_seconds <= MAX_SUPERVISED_DEADLINE_SECONDS
        or not 0 < policy.max_turns <= MAX_SUPERVISED_TURNS
    ):
        msg = 'Rehearsal budgets outside supervised limits'
        raise ValueError(msg)

    prepare_worktree(task, context.source_repo)
    plan = prepare_supervised_plan(task.brief, policy)
    worker = delegate_in_worktree(task, agents.worker)
    verified_audit(task)
    reviewer = review_worktree(task, agents.reviewer)
    approval = request_worktree_approval(task)

    return RehearsalResult(
        task=OrchestrationResult(
            OrchestrationState.AWAITING_APPROVAL,
            task.brief.task_id,
            worker,
            reviewer,
            approval,
        ),
        plan=plan,
    )


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_SUPERVISED_DEADLINE_SECONDS',
    'MAX_SUPERVISED_TURNS',
    'DelegationError',
    'OrchestrationResult',
    'OrchestrationState',
    'Path',
    'RehearsalContext',
    'RehearsalResult',
    'dataclass',
    'delegate_in_worktree',
    'prepare_supervised_plan',
    'prepare_worktree',
    'request_worktree_approval',
    'review_worktree',
    'run_supervised_rehearsal',
    'verified_audit',
]
