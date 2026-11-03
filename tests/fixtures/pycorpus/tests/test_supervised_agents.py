"""Hermes supervised planning tests never launch a gateway or process."""

from pathlib import Path  # noqa: TC003 - host Python 3.13 runtime annotations

import pytest

from hermes_harness.agents.supervised_agents import SupervisedPolicy, prepare_supervised_plan
from hermes_harness.core.delegation import Brief, DelegationError
from hermes_harness.gateway.hermes_adapter import HermesSafetyError


def _brief() -> Brief:
    return Brief(
        'test-1', 'Change one fixture', ('fixtures/example.txt',), 'worker-a', 'reviewer-b'
    )


def _policy(path: Path) -> SupervisedPolicy:
    return SupervisedPolicy(authorized_task_id='test-1', authorized_workspace=path)


def test_distinct_disabled_invocations(tmp_path: Path) -> None:
    plan = prepare_supervised_plan(_brief(), _policy(tmp_path))
    assert plan.worker.ticket_id != plan.reviewer.ticket_id
    assert plan.worker.prompt != plan.reviewer.prompt
    assert plan.worker.workspace == str(tmp_path)
    assert plan.reviewer.workspace == str(tmp_path)
    assert not plan.worker.execution_enabled
    assert not plan.reviewer.execution_enabled
    with pytest.raises(HermesSafetyError, match='execution disabled'):
        plan.require_executable()


def test_requires_explicit_task_authorization(tmp_path: Path) -> None:
    policy = SupervisedPolicy(authorized_task_id='another', authorized_workspace=tmp_path)
    with pytest.raises(PermissionError, match='Task-specific authorization'):
        prepare_supervised_plan(_brief(), policy)


def test_rejects_same_role_identity(tmp_path: Path) -> None:
    brief = Brief('test-1', 'Change fixture', ('fixtures/a',), 'same', 'same')
    with pytest.raises(DelegationError, match='identities must differ'):
        prepare_supervised_plan(brief, _policy(tmp_path))


def test_rejects_excessive_budget(tmp_path: Path) -> None:
    policy = SupervisedPolicy(
        authorized_task_id='test-1', authorized_workspace=tmp_path, deadline_seconds=301
    )
    with pytest.raises(ValueError, match='budgets'):
        prepare_supervised_plan(_brief(), policy)


def test_rejects_missing_workspace(tmp_path: Path) -> None:
    policy = _policy(tmp_path / 'missing')
    with pytest.raises(ValueError, match='workspace'):
        prepare_supervised_plan(_brief(), policy)


def test_rejects_symlink_workspace(tmp_path: Path) -> None:
    target = tmp_path / 'target'
    target.mkdir()
    link = tmp_path / 'link'
    link.symlink_to(target, target_is_directory=True)
    with pytest.raises(ValueError, match='workspace'):
        prepare_supervised_plan(_brief(), _policy(link))
