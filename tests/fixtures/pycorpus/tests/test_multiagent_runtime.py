"""Integrated deterministic multi-worktree flow and fail-closed validation."""

import os
import shutil
import subprocess
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import (
    DemoFactory,
    ExecutionConfig,
    Node,
    Plan,
    WorkflowError,
    decide,
    execute,
    inspect,
    ordered_nodes,
)

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.core.delegation import OfflineAgent


def _git(repo: Path, *args: str) -> None:
    binary = shutil.which('git')
    assert binary is not None
    subprocess.run(  # noqa: S603 - fixed Git invocation in temporary test repository
        [os.path.realpath(binary), '-C', str(repo), *args],
        check=True,
        shell=False,
    )


def _repo(root: Path) -> Path:
    repo = root / 'repo'
    repo.mkdir()
    _git(repo, 'init', '-q', '-b', 'dev')
    _git(
        repo,
        '-c',
        'user.name=Test',
        '-c',
        'user.email=test@example.invalid',
        'commit',
        '--allow-empty',
        '-qm',
        'initial',
    )
    return repo


def _plan() -> Plan:
    return Plan(
        'run',
        'Two edits',
        (
            Node('b', 'second', ('b.txt',), ('a',)),
            Node('a', 'first', ('a.txt',)),
        ),
    )


def test_full_temporary_git_repo_flow(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    state = tmp_path / 'state'
    state.mkdir()
    assert [node.id for node in ordered_nodes(_plan())] == ['a', 'b']
    result = execute(state, _plan(), ExecutionConfig(repo, DemoFactory()))
    assert result['state'] == 'pending_human_decision'
    assert [node['state'] for node in result['tasks']] == ['completed', 'completed']
    assert (state / 'run/workspaces/a/a.txt').read_text() == 'offline evidence for a\n'
    assert (state / 'run/workspaces/b/b.txt').read_text() == 'offline evidence for b\n'
    assert not (repo / 'a.txt').exists()
    assert inspect(state, 'run')['decision'] == {
        'state': 'pending',
        'approved': False,
        'integrated': False,
        'run_id': 'run',
    }
    assert decide(state, 'run', 'reject')['applied'] is False
    with pytest.raises(FileExistsError):
        decide(state, 'run', 'approve')
    with pytest.raises(FileExistsError):
        execute(state, _plan(), ExecutionConfig(repo, DemoFactory()))


@pytest.mark.parametrize(
    'nodes',
    [
        (Node('a', 'test', ('a',), ('b',)), Node('b', 'test', ('b',), ('a',))),
        (Node('a', 'test', ('x',)), Node('b', 'test', ('x/y',))),
        (Node('a', 'test', ('../escape',)),),
        (Node('a', 'test', ('.git/config',)),),
        (Node('a', 'test', ('a',), ('missing',)),),
    ],
)
def test_rejects_unsafe_plans(nodes: tuple[Node, ...]) -> None:
    with pytest.raises(WorkflowError):
        ordered_nodes(Plan('run', 'test', nodes))


def test_failure_is_fenced_without_retry(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    state = tmp_path / 'state'
    state.mkdir()

    class FailingFactory(DemoFactory):
        def worker(self, node: Node, workspace: Path) -> OfflineAgent:
            _ = (node, workspace)
            msg = 'simulated failure before dispatch'
            raise RuntimeError(msg)

    with pytest.raises(RuntimeError):
        execute(state, _plan(), ExecutionConfig(repo, FailingFactory()))
    assert inspect(state, 'run')['state'] == 'fenced_or_incomplete'
    with pytest.raises(FileExistsError):
        execute(state, _plan(), ExecutionConfig(repo, DemoFactory()))
