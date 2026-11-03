"""Regression tests for the Hermes skill bridge over the canonical runtime."""

import json
import shutil
import subprocess
from pathlib import Path

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.workflows import (
    assemble,
    game_plan,
    inspect_run,
    investigate,
    parse_plan,
)


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / 'repo'
    repo.mkdir()
    git = shutil.which('git')
    assert git is not None
    subprocess.run([git, '-C', str(repo), 'init', '-q'], check=True, shell=False)  # noqa: S603
    subprocess.run(  # noqa: S603
        [
            git,
            '-C',
            str(repo),
            '-c',
            'user.name=Test',
            '-c',
            'user.email=test@example.invalid',
            'commit',
            '--allow-empty',
            '-qm',
            'baseline',
        ],
        check=True,
        shell=False,
    )
    return repo


def _plan(tmp_path: Path) -> Path:
    path = tmp_path / 'plan.json'
    path.write_text(
        json.dumps({
            'id': 'offline-01',
            'objective': 'offline evidence',
            'nodes': [
                {'id': 'a', 'objective': 'create evidence', 'owned_paths': ['evidence.txt']},
                {
                    'id': 'b',
                    'objective': 'create more',
                    'owned_paths': ['more.txt'],
                    'dependencies': ['a'],
                },
            ],
        }),
        encoding='utf-8',
    )
    return path


def test_investigate_reads_repo_metadata(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    result = investigate(repo)
    assert result['mode'] == 'read-only'
    assert result['tracked_count'] == 0
    assert len(str(result['head'])) == len('a' * 40)


def test_investigate_rejects_not_repo(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='Git checkout'):
        investigate(tmp_path)


def test_game_plan_uses_real_dag(tmp_path: Path) -> None:
    result = game_plan(_plan(tmp_path))
    assert result['execution_order'] == ['a', 'b']


def test_invalid_node_shape_is_rejected() -> None:
    with pytest.raises(WorkflowError, match='Invalid node fields'):
        parse_plan({
            'id': 'ok',
            'objective': 'ok',
            'nodes': [{'id': 'n', 'objective': 'ok', 'owned_paths': '../bad'}],
        })


def test_overlapping_paths_rejected() -> None:
    with pytest.raises(WorkflowError, match='Overlapping ownership'):
        parse_plan({
            'id': 'ok',
            'objective': 'ok',
            'nodes': [
                {'id': 'a', 'objective': 'ok', 'owned_paths': ['foo']},
                {'id': 'b', 'objective': 'ok', 'owned_paths': ['foo/bar']},
            ],
        })


def test_offline_dispatch_and_inspection(tmp_path: Path) -> None:
    repo = _repo(tmp_path)
    state = tmp_path / 'state'
    result = assemble(repo, state, _plan(tmp_path))
    assert result['mode'] == 'offline-simulated'
    assert result['result']['state'] == 'pending_human_decision'
    assert [row['state'] for row in result['result']['tasks']] == ['completed', 'completed']
    assert inspect_run(state, 'offline-01')['result']['state'] == 'pending_human_decision'
    with pytest.raises(FileExistsError):
        assemble(repo, state, _plan(tmp_path))


def test_plan_symlink_rejected(tmp_path: Path) -> None:
    file = _plan(tmp_path)
    link = tmp_path / 'link.json'
    link.symlink_to(file)
    with pytest.raises(WorkflowError, match='Plan must be a real'):
        game_plan(link)


def test_skills_exist() -> None:
    root = Path(__file__).resolve().parents[1] / 'hermes_skills'
    for name in ('lets-investigate', 'game-plan', 'agents-assemble'):
        contents = (root / name / 'SKILL.md').read_text()
        assert f'name: {name}' in contents
        assert 'hermes_harness.native' in contents
