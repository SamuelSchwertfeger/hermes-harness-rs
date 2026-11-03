"""Offline immutable-commit CI request tests; never connects to CI providers."""

import json
from pathlib import Path  # noqa: TC003 - used by fixture annotations
from typing import TYPE_CHECKING

import pytest

from hermes_harness.ci.ci_request import create_ci_request, inspect_ci_request
from hermes_harness.core.delegation import DelegationError, Finding
from hermes_harness.git.worktree_delegation import (
    delegate_in_worktree,
    prepare_worktree,
    review_worktree,
)
from hermes_harness.git.worktrees import _git
from test_task_reservation import CountingAgent

pytest_plugins = ('test_task_reservation',)

if TYPE_CHECKING:
    from hermes_harness.git.worktree_delegation import (
        TaskWorkspace,
    )


def _reviewed(task_repo: tuple[TaskWorkspace, Path]) -> TaskWorkspace:
    task, repo = task_repo
    prepare_worktree(task, repo)

    class Worker:
        def run(self, _brief: object, _prior: object) -> Finding:
            path = task.workspace / 'src' / 'safe.py'
            path.parent.mkdir()
            path.write_text('reviewed candidate\n')
            return Finding('worker', 'offline work')

    delegate_in_worktree(task, Worker())
    review_worktree(task, CountingAgent('reviewer'))
    return task


def test_request_requires_clean_commit(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task = _reviewed(task_repo)
    with pytest.raises(DelegationError, match='committed and clean'):
        create_ci_request(task)
    _git(task.workspace, 'add', 'src/safe.py')
    _git(
        task.workspace,
        '-c',
        'user.name=Offline',
        '-c',
        'user.email=offline@example.invalid',
        'commit',
        '-m',
        'offline candidate',
    )
    request = create_ci_request(task)
    assert request.commit == _git(task.workspace, 'rev-parse', 'HEAD')
    assert request.tree == _git(task.workspace, 'rev-parse', 'HEAD^{tree}')
    assert inspect_ci_request(task) == request
    with pytest.raises(FileExistsError):
        create_ci_request(task)


def test_request_rejects_changed_content(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task = _reviewed(task_repo)
    (task.workspace / 'src/safe.py').write_text('tampered\n')
    _git(task.workspace, 'add', 'src/safe.py')
    _git(
        task.workspace,
        '-c',
        'user.name=Offline',
        '-c',
        'user.email=offline@example.invalid',
        'commit',
        '-m',
        'tampered candidate',
    )
    with pytest.raises(DelegationError, match='snapshot'):
        create_ci_request(task)


def test_request_is_not_ci_result_and_detects_tampering(
    task_repo: tuple[TaskWorkspace, Path],
) -> None:
    task = _reviewed(task_repo)
    _git(task.workspace, 'add', 'src/safe.py')
    _git(
        task.workspace,
        '-c',
        'user.name=Offline',
        '-c',
        'user.email=offline@example.invalid',
        'commit',
        '-m',
        'offline candidate',
    )
    create_ci_request(task)
    path = task.state_root / task.brief.task_id / 'ci-request.json'
    value = json.loads(path.read_text())
    assert 'passed' not in value
    assert 'approved' not in value
    value['candidate']['tree'] = '0' * 40
    path.write_text(json.dumps(value))
    with pytest.raises(DelegationError, match='checksum'):
        inspect_ci_request(task)


def test_candidate_refuses_head_change(task_repo: tuple[TaskWorkspace, Path]) -> None:
    task = _reviewed(task_repo)
    _git(task.workspace, 'add', 'src/safe.py')
    _git(
        task.workspace,
        '-c',
        'user.name=Offline',
        '-c',
        'user.email=offline@example.invalid',
        'commit',
        '-m',
        'offline candidate',
    )
    create_ci_request(task)
    _git(
        task.workspace,
        '-c',
        'user.name=Offline',
        '-c',
        'user.email=offline@example.invalid',
        'commit',
        '--allow-empty',
        '-m',
        'new head',
    )
    with pytest.raises(DelegationError, match='no longer matches'):
        inspect_ci_request(task)
