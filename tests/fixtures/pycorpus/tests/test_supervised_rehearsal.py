"""End-to-end supervised rehearsal with real disposable Git worktrees."""

import shutil
import subprocess
from pathlib import Path  # noqa: TC003 - compatible runtime annotations

import pytest

from hermes_harness.agents.supervised_agents import SupervisedPolicy
from hermes_harness.agents.supervised_rehearsal import RehearsalContext, run_supervised_rehearsal
from hermes_harness.core.delegation import Brief, Finding
from hermes_harness.git.worktree_delegation import TaskWorkspace
from hermes_harness.supervised.offline_orchestrator import (
    OfflineAgents,
    OrchestrationState,
    inspect_offline_task,
)


class LocalDouble:
    def __init__(self, identity: str, workspace: Path, *, write: bool = False) -> None:
        self.identity = identity
        self.workspace = workspace
        self.write = write
        self.calls = 0

    def run(self, _brief: Brief, prior: Finding | None) -> Finding:
        self.calls += 1
        if self.write:
            target = self.workspace / 'src' / 'safe.py'
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text('offline fixture\n')
        return Finding(self.identity, 'review' if prior else 'work')


@pytest.fixture
def setup(tmp_path: Path) -> tuple[TaskWorkspace, Path, SupervisedPolicy, LocalDouble, LocalDouble]:
    git = shutil.which('git')
    assert git is not None
    repo = tmp_path / 'repo'
    repo.mkdir()
    for args in [
        ('init',),
        ('config', 'user.email', 'test@example.invalid'),
        ('config', 'user.name', 'Tester'),
    ]:
        subprocess.run(  # noqa: S603 - disposable Git test repository
            [git, '-C', str(repo), *args], check=True, capture_output=True
        )
    (repo / 'README.md').write_text('fixture\n')
    for args in [('add', 'README.md'), ('commit', '-m', 'base')]:
        subprocess.run(  # noqa: S603 - disposable Git test repository
            [git, '-C', str(repo), *args], check=True, capture_output=True
        )
    root = tmp_path / 'state'
    root.mkdir()
    workspace = tmp_path / 'worktree'
    brief = Brief('rehearsal-one', 'Offline rehearsal', ('src/safe.py',), 'worker', 'reviewer')
    task = TaskWorkspace(root, workspace, brief)
    policy = SupervisedPolicy(authorized_task_id=brief.task_id, authorized_workspace=workspace)
    return (
        task,
        repo,
        policy,
        LocalDouble('worker', workspace, write=True),
        LocalDouble('reviewer', workspace),
    )


def test_rehearsal_success_and_disabled_plan(
    setup: tuple[TaskWorkspace, Path, SupervisedPolicy, LocalDouble, LocalDouble],
) -> None:
    task, repo, policy, worker, reviewer = setup
    result = run_supervised_rehearsal(
        task, OfflineAgents(worker, reviewer), RehearsalContext(repo, policy)
    )
    assert result.task.state is OrchestrationState.AWAITING_APPROVAL
    assert worker.calls == reviewer.calls == 1
    assert not result.plan.worker.execution_enabled
    assert not result.plan.reviewer.execution_enabled
    assert result.plan.worker.workspace == str(task.workspace)
    assert (task.state_root / task.brief.task_id / 'approval-request.json').read_text().find(
        'false'
    ) >= 0
    assert inspect_offline_task(task).state is OrchestrationState.AWAITING_APPROVAL
    with pytest.raises((FileExistsError, RuntimeError, OSError)):
        run_supervised_rehearsal(
            task, OfflineAgents(worker, reviewer), RehearsalContext(repo, policy)
        )
    assert worker.calls == reviewer.calls == 1


def test_rehearsal_rejects_wrong_authorization_before_reservation(
    setup: tuple[TaskWorkspace, Path, SupervisedPolicy, LocalDouble, LocalDouble],
) -> None:
    task, repo, _policy, worker, reviewer = setup
    policy = SupervisedPolicy(authorized_task_id='wrong', authorized_workspace=task.workspace)
    with pytest.raises(PermissionError):
        run_supervised_rehearsal(
            task, OfflineAgents(worker, reviewer), RehearsalContext(repo, policy)
        )
    assert not (task.state_root / task.brief.task_id).exists()


def test_rehearsal_rejects_same_agent_before_reservation(
    setup: tuple[TaskWorkspace, Path, SupervisedPolicy, LocalDouble, LocalDouble],
) -> None:
    task, repo, policy, worker, _reviewer = setup
    with pytest.raises(RuntimeError):
        run_supervised_rehearsal(
            task, OfflineAgents(worker, worker), RehearsalContext(repo, policy)
        )
    assert not (task.state_root / task.brief.task_id).exists()


def test_worker_exception_stays_uncertain_without_retry(
    setup: tuple[TaskWorkspace, Path, SupervisedPolicy, LocalDouble, LocalDouble],
) -> None:
    task, repo, policy, worker, reviewer = setup

    class FailingDouble(LocalDouble):
        def run(self, _brief: Brief, _prior: Finding | None) -> Finding:
            self.calls += 1
            msg = 'simulated worker failure'
            raise RuntimeError(msg)

    worker = FailingDouble('worker', task.workspace)
    with pytest.raises(RuntimeError, match='simulated worker failure'):
        run_supervised_rehearsal(
            task, OfflineAgents(worker, reviewer), RehearsalContext(repo, policy)
        )
    assert reviewer.calls == 0
    assert inspect_offline_task(task).state is OrchestrationState.DISPATCH_UNCERTAIN
    with pytest.raises((FileExistsError, RuntimeError, OSError)):
        run_supervised_rehearsal(
            task, OfflineAgents(worker, reviewer), RehearsalContext(repo, policy)
        )
    assert worker.calls == 1
