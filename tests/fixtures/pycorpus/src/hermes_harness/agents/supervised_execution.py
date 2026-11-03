"""One-shot scripted supervised gateway orchestration over real Git evidence.

This module is strictly for offline qualification. A controlled worker double
applies changes after gateway receipt; it is NOT a live Hermes executor.
"""

import sqlite3
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.agents.supervised_agents import prepare_supervised_plan
from hermes_harness.agents.supervised_review_handoff import (
    HandoffSources,
    prepare_reviewer_handoff_once,
    verify_reviewer_handoff,
)
from hermes_harness.agents.supervised_reviewer_dispatch import (
    ReviewerDispatchInputs,
    RuntimeBinding,
    submit_reviewer_once,
)
from hermes_harness.core.delegation import DelegationError
from hermes_harness.gateway.admission_store import AdmissionStore
from hermes_harness.gateway.gateway_live_admission import LiveAdmissionRequest, submit_once
from hermes_harness.gateway.gateway_terminal_reconciliation import reconcile_terminal_once
from hermes_harness.gateway.scripted_supervised_gateway import ScriptedSupervisedGateway
from hermes_harness.gateway.supervised_gateway_receipts import RoleAdmissions
from hermes_harness.git.worktree_delegation import (
    delegate_in_worktree,
    prepare_worktree,
    request_worktree_approval,
    review_worktree,
    verified_audit,
)
from hermes_harness.supervised.offline_orchestrator import inspect_offline_task

if TYPE_CHECKING:
    from hermes_harness.agents.supervised_agents import SupervisedPlan, SupervisedPolicy
    from hermes_harness.agents.supervised_reviewer_dispatch import RuntimeQualifier
    from hermes_harness.core.delegation import OfflineAgent
    from hermes_harness.gateway.gateway_identity import Submission
    from hermes_harness.gateway.gateway_live_admission import AsyncGatewayRPC
    from hermes_harness.git.worktree_delegation import TaskWorkspace


class ExecutionBlockedError(RuntimeError):
    """Supervised demonstration cannot advance safely."""


@dataclass(frozen=True, slots=True, kw_only=True)
class ScriptedExecution:
    """Explicit test-only inputs; caller controls both deterministic agents."""

    task: TaskWorkspace
    source_repo: Path
    policy: SupervisedPolicy
    worker_submission: Submission
    reviewer_submission: Submission
    ledger_path: Path
    rpc: AsyncGatewayRPC
    qualifier: RuntimeQualifier
    worker: OfflineAgent
    reviewer: OfflineAgent


@dataclass(frozen=True, slots=True)
class ExecutionResult:
    """Successful scripted flow; approval remains unapproved."""

    worker_admission_id: str
    reviewer_admission_id: str
    audit_sha256: str
    approval_request: Path


def _check_identity(config: ScriptedExecution) -> None:
    if config.policy.authorized_task_id != config.task.brief.task_id:
        msg = 'Policy task identity mismatch'
        raise ExecutionBlockedError(msg)
    if config.policy.authorized_workspace != config.task.workspace:
        msg = 'Policy workspace mismatch'
        raise ExecutionBlockedError(msg)
    if config.worker_submission.session_id == config.reviewer_submission.session_id:
        msg = 'Worker and reviewer sessions must differ'
        raise ExecutionBlockedError(msg)
    if config.worker_submission.submission_id == config.reviewer_submission.submission_id:
        msg = 'Worker and reviewer submission IDs must differ'
        raise ExecutionBlockedError(msg)
    if config.worker is config.reviewer:
        msg = 'Worker and reviewer implementations must differ'
        raise ExecutionBlockedError(msg)


def _check(config: ScriptedExecution) -> None:
    if not isinstance(config.rpc, ScriptedSupervisedGateway):
        msg = 'Only the built-in network-free scripted gateway is permitted'
        raise ExecutionBlockedError(msg)
    _check_identity(config)
    if config.worker_submission.admission_id or config.reviewer_submission.admission_id:
        msg = 'Only fresh submissions are permitted'
        raise ExecutionBlockedError(msg)
    if config.ledger_path.exists():
        msg = 'Existing ledger requires read-only inspection, never fresh execution'
        raise ExecutionBlockedError(msg)


@dataclass(frozen=True, slots=True)
class _ExecutionContext:
    config: ScriptedExecution
    plan: SupervisedPlan
    store: AdmissionStore
    ids: RoleAdmissions


async def _run_worker(context: _ExecutionContext) -> None:
    config = context.config
    plan = context.plan
    # The same sandbox attestation is required at the worker boundary.
    config.qualifier.require_qualified(
        RuntimeBinding(
            task_id=plan.task_id,
            reviewer_id=plan.worker_id,
            session_id=config.worker_submission.session_id,
            workspace=config.task.workspace.resolve(strict=True),
            audit_sha256='pre-worker',
        )
    )
    await submit_once(
        context.store,
        LiveAdmissionRequest(config.worker_submission, plan.worker.prompt),
        rpc=config.rpc,
    )
    receipt = await reconcile_terminal_once(
        config.rpc, context.store, config.worker_submission.submission_id
    )
    if receipt.outcome != 'completed' or not receipt.final_response:
        msg = 'Worker receipt is not a completed nonempty response'
        raise ExecutionBlockedError(msg)
    # The deterministic double, not the gateway response, edits the Git worktree.
    worker = delegate_in_worktree(config.task, config.worker)
    if worker.agent_id != plan.worker_id:
        msg = 'Worker evidence identity mismatch'
        raise DelegationError(msg)
    verified_audit(config.task)


async def _run_reviewer(context: _ExecutionContext) -> str:
    config = context.config
    plan = context.plan
    handoff = await prepare_reviewer_handoff_once(
        plan,
        config.task,
        HandoffSources(ids=context.ids, store=context.store, rpc=config.rpc),
    )
    verify_reviewer_handoff(config.task, handoff)
    await submit_reviewer_once(
        ReviewerDispatchInputs(
            task=config.task,
            handoff=handoff,
            submission=config.reviewer_submission,
            store=context.store,
            rpc=config.rpc,
            qualifier=config.qualifier,
        )
    )
    receipt = await reconcile_terminal_once(
        config.rpc, context.store, config.reviewer_submission.submission_id
    )
    if receipt.outcome != 'completed' or not receipt.final_response:
        msg = 'Reviewer receipt is not a completed nonempty response'
        raise ExecutionBlockedError(msg)
    verify_reviewer_handoff(config.task, handoff)
    reviewer = review_worktree(config.task, config.reviewer)
    if reviewer.agent_id != plan.reviewer_id or not reviewer.text.strip():
        msg = 'Independent reviewer finding missing'
        raise ExecutionBlockedError(msg)
    return handoff.audit_sha256


async def execute_scripted_once(config: ScriptedExecution) -> ExecutionResult:
    """Run one offline trace, never resume or replay uncertain submissions."""
    _check(config)
    prepare_worktree(config.task, config.source_repo)
    plan = prepare_supervised_plan(config.task.brief, config.policy)
    context = _ExecutionContext(
        config=config,
        plan=plan,
        store=AdmissionStore(config.ledger_path),
        ids=RoleAdmissions(
            task_id=config.task.brief.task_id,
            worker_submission_id=config.worker_submission.submission_id,
            reviewer_submission_id=config.reviewer_submission.submission_id,
        ),
    )
    await _run_worker(context)
    audit_sha256 = await _run_reviewer(context)
    approval = request_worktree_approval(config.task)
    worker_record = context.store.get(config.worker_submission.submission_id)
    reviewer_record = context.store.get(config.reviewer_submission.submission_id)
    if worker_record is None or reviewer_record is None:
        msg = 'Missing durable admissions after reconciliation'
        raise ExecutionBlockedError(msg)

    if worker_record.admission_id is None or reviewer_record.admission_id is None:
        msg = 'Missing durable admissions after reconciliation'
        raise ExecutionBlockedError(msg)
    return ExecutionResult(
        worker_record.admission_id,
        reviewer_record.admission_id,
        audit_sha256,
        approval,
    )


def inspect_scripted(task: TaskWorkspace, ledger_path: Path) -> dict[str, object]:
    """Read-only progress inspection; never invokes gateway or dispatch."""
    status = inspect_offline_task(task)
    if not ledger_path.is_file() or ledger_path.is_symlink():
        return {'task_id': task.brief.task_id, 'phase': status.state.value, 'ledger': 'missing'}
    # SQLite opened explicitly read-only: do not construct AdmissionStore, whose
    # initialization may create the database or change its permissions.
    with sqlite3.connect(f'file:{ledger_path}?mode=ro', uri=True) as connection:
        rows = connection.execute(
            'SELECT submission_id, local_state, admission_id FROM admissions '
            'ORDER BY submission_id LIMIT 100'
        ).fetchall()
    return {
        'task_id': task.brief.task_id,
        'phase': status.state.value,
        'admissions': [
            {'submission_id': row[0], 'state': row[1], 'admission_id': row[2]} for row in rows
        ],
    }


# Temporary exports for legacy module import compatibility.
__all__ = [
    'AdmissionStore',
    'DelegationError',
    'ExecutionBlockedError',
    'ExecutionResult',
    'HandoffSources',
    'LiveAdmissionRequest',
    'Path',
    'ReviewerDispatchInputs',
    'RoleAdmissions',
    'RuntimeBinding',
    'ScriptedExecution',
    'ScriptedSupervisedGateway',
    '_ExecutionContext',
    '_check',
    '_check_identity',
    '_run_reviewer',
    '_run_worker',
    'dataclass',
    'delegate_in_worktree',
    'execute_scripted_once',
    'inspect_offline_task',
    'inspect_scripted',
    'prepare_reviewer_handoff_once',
    'prepare_supervised_plan',
    'prepare_worktree',
    'reconcile_terminal_once',
    'request_worktree_approval',
    'review_worktree',
    'sqlite3',
    'submit_once',
    'submit_reviewer_once',
    'verified_audit',
    'verify_reviewer_handoff',
]
