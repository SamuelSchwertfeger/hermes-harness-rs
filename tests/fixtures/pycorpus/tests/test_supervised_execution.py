"""Disposable Git end-to-end supervised scripted gateway qualification."""

import asyncio
import json
import subprocess
from dataclasses import replace
from pathlib import Path  # noqa: TC003
from typing import TYPE_CHECKING

import pytest

from hermes_harness.agents.supervised_agents import SupervisedPolicy
from hermes_harness.agents.supervised_execution import (
    ExecutionBlockedError,
    ScriptedExecution,
    execute_scripted_once,
    inspect_scripted,
)
from hermes_harness.agents.supervised_reviewer_dispatch import DisabledRuntimeQualifier
from hermes_harness.core.delegation import Brief, Finding
from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_identity import GatewayOutcomeUnknownError, Submission
from hermes_harness.gateway.scripted_supervised_gateway import ScriptedSupervisedGateway
from hermes_harness.git.worktree_delegation import TaskWorkspace

if TYPE_CHECKING:
    from hermes_harness.agents.supervised_reviewer_dispatch import RuntimeBinding

EXPECTED_ROLE_SUBMISSIONS = 2


class QualifiedScripted:
    def __init__(self) -> None:
        self.roles: list[str] = []

    def require_qualified(self, binding: RuntimeBinding) -> None:
        self.roles.append(binding.reviewer_id)


class Worker:
    def __init__(self, workspace: Path) -> None:
        self.workspace = workspace

    def run(self, brief: Brief, prior: Finding | None) -> Finding:
        assert prior is None
        target = self.workspace / 'src' / 'changed.txt'
        target.parent.mkdir(exist_ok=True)
        target.write_text('worker output\n')
        return Finding(brief.worker_id, 'worker implementation')


class Reviewer:
    def run(self, brief: Brief, prior: Finding | None) -> Finding:
        assert prior is not None
        assert prior.agent_id == brief.worker_id
        return Finding(brief.reviewer_id, 'independent review: requires human approval')


def _config(tmp_path: Path, gateway: ScriptedSupervisedGateway) -> ScriptedExecution:
    repo = tmp_path / 'repo'
    repo.mkdir()
    for args in (
        ('init',),
        ('config', 'user.email', 'offline@example.invalid'),
        ('config', 'user.name', 'Offline'),
    ):
        subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True)  # noqa: S603, S607
    (repo / 'README.md').write_text('baseline\n')
    for args in (('add', 'README.md'), ('commit', '-m', 'baseline')):
        subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True)  # noqa: S603, S607
    state = tmp_path / 'state'
    state.mkdir()
    workspace = tmp_path / 'workspace'
    brief = Brief('integrated', 'Offline qualification', ('src/changed.txt',), 'worker', 'reviewer')
    return ScriptedExecution(
        task=TaskWorkspace(state, workspace, brief),
        source_repo=repo,
        policy=SupervisedPolicy(authorized_task_id=brief.task_id, authorized_workspace=workspace),
        worker_submission=Submission('worker-session', 'worker-sub'),
        reviewer_submission=Submission('review-session', 'review-sub'),
        ledger_path=tmp_path / 'admissions.sqlite',
        rpc=gateway,
        qualifier=QualifiedScripted(),
        worker=Worker(workspace),
        reviewer=Reviewer(),
    )


def test_end_to_end_and_duplicate_fence(tmp_path: Path) -> None:
    gateway = ScriptedSupervisedGateway()
    config = _config(tmp_path, gateway)
    result = asyncio.run(execute_scripted_once(config))
    assert result.worker_admission_id != result.reviewer_admission_id
    assert json.loads(result.approval_request.read_text())['approved'] is False
    assert gateway.calls.count('prompt.submit') == EXPECTED_ROLE_SUBMISSIONS
    assert config.qualifier.roles == ['worker', 'reviewer']
    assert inspect_scripted(config.task, config.ledger_path)['phase'] == 'awaiting_approval'
    with pytest.raises(ExecutionBlockedError, match='Existing ledger'):
        asyncio.run(execute_scripted_once(config))
    assert gateway.calls.count('prompt.submit') == EXPECTED_ROLE_SUBMISSIONS


def test_worker_failure_blocks_reviewer(tmp_path: Path) -> None:
    gateway = ScriptedSupervisedGateway(worker_outcome='failed')
    config = _config(tmp_path, gateway)
    with pytest.raises(ExecutionBlockedError, match='Worker receipt'):
        asyncio.run(execute_scripted_once(config))
    assert gateway.calls.count('prompt.submit') == 1
    assert not (config.task.workspace / 'src' / 'changed.txt').exists()


def test_uncertain_worker_submission_fenced(tmp_path: Path) -> None:
    gateway = ScriptedSupervisedGateway(fail_submission_role='worker')
    config = _config(tmp_path, gateway)
    with pytest.raises(GatewayOutcomeUnknownError):
        asyncio.run(execute_scripted_once(config))
    record = AdmissionStore(config.ledger_path).get('worker-sub')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    with pytest.raises(ExecutionBlockedError):
        asyncio.run(execute_scripted_once(config))
    assert gateway.calls.count('prompt.submit') == 1


def test_denied_qualifier_never_submits(tmp_path: Path) -> None:
    gateway = ScriptedSupervisedGateway()
    config = replace(_config(tmp_path, gateway), qualifier=DisabledRuntimeQualifier())
    with pytest.raises(Exception, match='disabled'):
        asyncio.run(execute_scripted_once(config))
    assert gateway.calls == []


@pytest.mark.parametrize(
    ('field', 'value'),
    [
        ('worker_submission', Submission('review-session', 'another')),
        ('reviewer_submission', Submission('review-session', 'worker-sub')),
    ],
)
def test_identity_refusal(tmp_path: Path, field: str, value: Submission) -> None:
    config = _config(tmp_path, ScriptedSupervisedGateway())
    with pytest.raises(ExecutionBlockedError):
        asyncio.run(execute_scripted_once(replace(config, **{field: value})))


def test_reviewer_failure_never_requests_approval(tmp_path: Path) -> None:
    gateway = ScriptedSupervisedGateway(reviewer_outcome='failed')
    config = _config(tmp_path, gateway)
    with pytest.raises(ExecutionBlockedError, match='Reviewer receipt'):
        asyncio.run(execute_scripted_once(config))
    assert gateway.calls.count('prompt.submit') == EXPECTED_ROLE_SUBMISSIONS
    assert not (config.task.state_root / 'integrated' / 'approval-request.json').exists()
    assert inspect_scripted(config.task, config.ledger_path)['phase'] != 'awaiting_approval'


def test_non_scripted_gateway_fails_before_worktree(tmp_path: Path) -> None:
    config = _config(tmp_path, ScriptedSupervisedGateway())
    with pytest.raises(ExecutionBlockedError, match='network-free'):
        asyncio.run(execute_scripted_once(replace(config, rpc=object())))  # type: ignore[arg-type]
    assert not config.task.workspace.exists()


def test_readonly_status_does_not_create_ledger(tmp_path: Path) -> None:
    from hermes_harness.agents.supervised_status import inspect  # noqa: PLC0415

    root = tmp_path / 'missing'
    result = inspect(root, 'scripted-demo')
    assert result['admissions'] == []
    assert result['approved'] is None
    assert not root.exists()


def test_readonly_status_after_success(tmp_path: Path) -> None:
    from hermes_harness.agents.supervised_status import inspect  # noqa: PLC0415

    config = _config(tmp_path, ScriptedSupervisedGateway())
    asyncio.run(execute_scripted_once(config))
    before = config.ledger_path.stat().st_mtime_ns
    status = inspect(tmp_path, 'integrated')
    assert status['approved'] is False
    assert len(status['admissions']) == EXPECTED_ROLE_SUBMISSIONS
    assert config.ledger_path.stat().st_mtime_ns == before
