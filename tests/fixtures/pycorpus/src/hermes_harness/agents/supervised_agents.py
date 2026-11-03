"""Preflight-only bridge from delegated tasks to disabled Hermes invocations.

Plans never invoke Hermes, providers, shells, or gateway sessions. Admission
requires separately verified runtime isolation and cancellation in future work.
"""

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.core.delegation import Brief, DelegationError
from hermes_harness.gateway.hermes_adapter import (
    HermesSafetyError,
    TaskKind,
    TaskRequest,
    execute,
    prepare,
)

if TYPE_CHECKING:
    from hermes_harness.gateway.hermes_adapter import (
        PreparedInvocation,
    )


MAX_SUPERVISED_DEADLINE_SECONDS = 300
MAX_SUPERVISED_TURNS = 5


@dataclass(frozen=True, slots=True, kw_only=True)
class SupervisedPolicy:
    """Explicit task authorization and narrow resource budgets."""

    authorized_task_id: str
    authorized_workspace: Path
    deadline_seconds: int = 60
    max_turns: int = 2


@dataclass(frozen=True, slots=True)
class SupervisedPlan:
    """Separate disabled invocations for distinct worker and reviewer roles."""

    task_id: str
    worker_id: str
    reviewer_id: str
    worker: PreparedInvocation
    reviewer: PreparedInvocation

    def require_executable(self) -> None:
        """Fail closed: manifests alone never establish execution eligibility."""
        execute(self.worker)
        execute(self.reviewer)
        msg = 'Supervised launch requires separately qualified sandbox and gateway admission'
        raise HermesSafetyError(msg)


def prepare_supervised_plan(brief: Brief, policy: SupervisedPolicy) -> SupervisedPlan:
    """Translate one task into distinct disabled Hermes role requests.

    The preflight deliberately performs no reservation, dispatch, or writes.
    The reviewer remains a separate invocation, never a reused worker session.
    """
    if not brief.task_id or policy.authorized_task_id != brief.task_id:
        msg = 'Task-specific authorization is required'
        raise PermissionError(msg)
    if not brief.worker_id or not brief.reviewer_id or brief.worker_id == brief.reviewer_id:
        msg = 'Worker and reviewer identities must differ'
        raise DelegationError(msg)
    workspace = policy.authorized_workspace
    if workspace.is_symlink() or not workspace.is_absolute() or not workspace.is_dir():
        msg = 'Authorized workspace must be an existing, absolute real directory'
        raise ValueError(msg)
    if (
        policy.deadline_seconds > MAX_SUPERVISED_DEADLINE_SECONDS
        or policy.max_turns > MAX_SUPERVISED_TURNS
    ):
        msg = 'Supervised budgets exceed preflight limits'
        raise ValueError(msg)
    worker_prompt = (
        f'Worker identity: {brief.worker_id}\nObjective: {brief.objective}\n'
        f'Owned paths: {", ".join(brief.owned_paths)}\n'
        'Make no changes outside owned paths. Do not push, merge, or grant approval.'
    )
    review_prompt = (
        f'Reviewer identity: {brief.reviewer_id}\nObjective: {brief.objective}\n'
        f'Owned paths: {", ".join(brief.owned_paths)}\n'
        'Independently review recorded worker evidence after audit. '
        'Do not modify files, push, merge, or grant approval.'
    )
    worker = prepare(
        TaskRequest(
            ticket_id=f'{brief.task_id}-worker',
            kind=TaskKind.PLAN,
            prompt=worker_prompt,
            workspace=workspace,
            deadline_seconds=policy.deadline_seconds,
            max_turns=policy.max_turns,
        )
    )
    reviewer = prepare(
        TaskRequest(
            ticket_id=f'{brief.task_id}-reviewer',
            kind=TaskKind.REVIEW,
            prompt=review_prompt,
            workspace=workspace,
            deadline_seconds=policy.deadline_seconds,
            max_turns=policy.max_turns,
        )
    )
    return SupervisedPlan(brief.task_id, brief.worker_id, brief.reviewer_id, worker, reviewer)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_SUPERVISED_DEADLINE_SECONDS',
    'MAX_SUPERVISED_TURNS',
    'Brief',
    'DelegationError',
    'HermesSafetyError',
    'Path',
    'SupervisedPlan',
    'SupervisedPolicy',
    'TaskKind',
    'TaskRequest',
    'dataclass',
    'execute',
    'prepare',
    'prepare_supervised_plan',
]
