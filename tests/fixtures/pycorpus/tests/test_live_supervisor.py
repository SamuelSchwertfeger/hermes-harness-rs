"""One-shot gateway staging without simulated Git worker success."""

import asyncio
from dataclasses import replace
from pathlib import Path  # noqa: TC003

import pytest

from hermes_harness.agents.supervised_agents import SupervisedPolicy, prepare_supervised_plan
from hermes_harness.agents.supervised_reviewer_dispatch import (
    ReviewerDispatchBlockedError,
)
from hermes_harness.core.delegation import Brief, DelegationError
from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_identity import GatewayOutcomeUnknownError, Submission
from hermes_harness.gateway.scripted_supervised_gateway import ScriptedSupervisedGateway
from hermes_harness.gateway.supervised_gateway_receipts import RoleAdmissions
from hermes_harness.git.worktree_delegation import TaskWorkspace
from hermes_harness.supervised.live_supervisor import (
    LiveSupervisorBlockedError,
    LiveSupervisorInputs,
    inspect_live_worker,
    submit_live_reviewer_once,
    submit_live_worker_once,
)


class QualifiedTestRuntime:
    def require_qualified(self, _binding: object) -> None:
        """Local test seam only; not a production runtime attestation."""


def _inputs(tmp_path: Path, gateway: ScriptedSupervisedGateway) -> LiveSupervisorInputs:
    workspace = tmp_path / 'workspace'
    workspace.mkdir()
    brief = Brief('live-test', 'Test admission', ('src/',), 'worker', 'reviewer')
    task = TaskWorkspace(tmp_path / 'state', workspace, brief)
    policy = SupervisedPolicy(authorized_task_id=brief.task_id, authorized_workspace=workspace)
    return LiveSupervisorInputs(
        task=task,
        plan=prepare_supervised_plan(brief, policy),
        ids=RoleAdmissions(
            task_id=brief.task_id,
            worker_submission_id='worker-sub',
            reviewer_submission_id='reviewer-sub',
        ),
        worker_submission=Submission('worker-session', 'worker-sub'),
        reviewer_submission=Submission('reviewer-session', 'reviewer-sub'),
        store=AdmissionStore(tmp_path / 'admissions.sqlite'),
        rpc=gateway,
    )


def test_default_denies_before_submission(tmp_path: Path) -> None:
    gateway = ScriptedSupervisedGateway()
    inputs = _inputs(tmp_path, gateway)
    with pytest.raises(ReviewerDispatchBlockedError):
        asyncio.run(submit_live_worker_once(inputs))
    assert not gateway.calls
    assert inputs.store.get('worker-sub') is None


def test_one_shot_and_receipt_with_external_test_qualifier(tmp_path: Path) -> None:
    gateway = ScriptedSupervisedGateway()
    inputs = _inputs(tmp_path, gateway)
    inputs = replace(inputs, qualifier=QualifiedTestRuntime())
    asyncio.run(submit_live_worker_once(inputs))
    evidence = asyncio.run(inspect_live_worker(inputs))
    assert evidence.outcome == 'completed'
    with pytest.raises(LiveSupervisorBlockedError, match='already recorded'):
        asyncio.run(submit_live_worker_once(inputs))
    assert gateway.calls.count('prompt.submit') == 1
    with pytest.raises(DelegationError):
        asyncio.run(submit_live_reviewer_once(inputs))
    assert gateway.calls.count('prompt.submit') == 1


def test_unknown_outcome_no_replay(tmp_path: Path) -> None:
    gateway = ScriptedSupervisedGateway(fail_submission_role='worker')
    inputs = replace(_inputs(tmp_path, gateway), qualifier=QualifiedTestRuntime())
    with pytest.raises(GatewayOutcomeUnknownError):
        asyncio.run(submit_live_worker_once(inputs))
    record = inputs.store.get('worker-sub')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    with pytest.raises(LiveSupervisorBlockedError):
        asyncio.run(submit_live_worker_once(inputs))
    assert gateway.calls.count('prompt.submit') == 1
