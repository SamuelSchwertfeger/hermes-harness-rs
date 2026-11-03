"""Offline crash and contention regressions on disposable Git fixtures."""

import json
import os
import shutil
import subprocess
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from pathlib import Path  # noqa: TC003 - runtime annotations on Python 3.13

import pytest

from hermes_harness.core.delegation import Brief, DelegationError, Finding
from hermes_harness.core.task_reservation import RecoveryState, fence, reconcile, reserve
from hermes_harness.git.worktree_delegation import (
    TaskWorkspace,
    delegate_in_worktree,
    prepare_worktree,
    review_worktree,
)


class CountingAgent:
    def __init__(self, identity: str, *, fail: bool = False) -> None:
        self.identity = identity
        self.fail = fail
        self.calls = 0

    def run(self, _brief: Brief, _prior: Finding | None) -> Finding:
        self.calls += 1
        if self.fail:
            msg = 'simulated worker crash'
            raise RuntimeError(msg)
        return Finding(self.identity, 'offline only')


@pytest.fixture
def task_repo(tmp_path: Path) -> tuple[TaskWorkspace, Path]:
    repo = tmp_path / 'repo'
    repo.mkdir()
    git = shutil.which('git')
    assert git is not None
    for args in [
        ('init',),
        ('config', 'user.email', 'local@example.invalid'),
        ('config', 'user.name', 'Offline'),
    ]:
        subprocess.run(  # noqa: S603 - controlled disposable Git fixture
            [git, '-C', str(repo), *args],
            check=True,
            capture_output=True,
            timeout=15,
            env={**os.environ, 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_TERMINAL_PROMPT': '0'},
        )
    (repo / 'README.md').write_text('fixture\n')
    for args in [('add', '.'), ('commit', '-m', 'baseline')]:
        subprocess.run(  # noqa: S603 - controlled disposable Git fixture
            [git, '-C', str(repo), *args],
            check=True,
            capture_output=True,
            timeout=15,
            env={**os.environ, 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_TERMINAL_PROMPT': '0'},
        )
    state = tmp_path / 'state'
    state.mkdir()
    return TaskWorkspace(
        state,
        tmp_path / 'workspace',
        Brief('task-one', 'test', ('src/safe.py',), 'worker', 'reviewer'),
    ), repo


def test_reserved_crash_is_read_only(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task, repo = task_repo
    record = reserve(task, repo)
    assert record.baseline
    assert reconcile(task) is RecoveryState.RESERVED
    assert not task.workspace.exists()
    with pytest.raises(DelegationError, match='reserved'):
        reserve(task, repo)


def test_preparing_crash_never_retries(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task, repo = task_repo
    reserve(task, repo)
    fence(task, 'preparing')
    assert reconcile(task) is RecoveryState.PREPARING
    with pytest.raises(DelegationError):
        prepare_worktree(task, repo)


def test_uncertain_worker_must_not_repeat(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task, repo = task_repo
    prepare_worktree(task, repo)
    agent = CountingAgent('worker', fail=True)
    with pytest.raises(RuntimeError, match='crash'):
        delegate_in_worktree(task, agent)
    assert reconcile(task) is RecoveryState.DISPATCH_UNCERTAIN
    with pytest.raises(DelegationError, match='Dispatch requires'):
        delegate_in_worktree(task, agent)
    assert agent.calls == 1


def test_partial_worker_output_is_uncertain(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task, repo = task_repo
    prepare_worktree(task, repo)
    fence(task, 'dispatch')
    (task.state_root / task.brief.task_id / 'worker.json').write_text('{}')
    assert reconcile(task) is RecoveryState.DISPATCH_UNCERTAIN


def test_symlink_and_mismatched_brief_rejected(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task, repo = task_repo
    reserve(task, repo)
    changed = replace(task, brief=replace(task.brief, objective='changed'))
    with pytest.raises(DelegationError, match='digest'):
        reconcile(changed)
    path = task.state_root / task.brief.task_id / 'reservation.json'
    path.unlink()
    path.symlink_to(repo / 'README.md')
    with pytest.raises(DelegationError, match='Symlink'):
        reconcile(task)


def test_malformed_stage_requires_manual_intervention(
    task_repo: tuple[TaskWorkspace, Path],
) -> None:
    task, repo = task_repo
    reserve(task, repo)
    (task.state_root / task.brief.task_id / 'preparing.json').write_text(
        json.dumps({'stage': 'wrong'})
    )
    assert reconcile(task) is RecoveryState.MANUAL_INTERVENTION


def test_concurrent_reservers_only_one_wins(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task, repo = task_repo

    def attempt(_index: int) -> bool:
        try:
            reserve(task, repo)
        except DelegationError:
            return False
        return True

    with ThreadPoolExecutor(max_workers=6) as executor:
        assert sum(executor.map(attempt, range(6))) == 1
    assert reconcile(task) is RecoveryState.RESERVED


def test_prepared_and_worker_evidence(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task, repo = task_repo
    prepare_worktree(task, repo)
    assert reconcile(task) is RecoveryState.PREPARED
    delegate_in_worktree(task, CountingAgent('worker'))
    assert reconcile(task) is RecoveryState.WORKER_RECORDED
    review_worktree(task, CountingAgent('reviewer'))
    with pytest.raises(DelegationError, match='Reviewer already'):
        review_worktree(task, CountingAgent('reviewer'))
