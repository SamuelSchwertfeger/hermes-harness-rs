"""Restricted disposable worktree proposal-application tests."""

import os
import shutil
import subprocess
from typing import TYPE_CHECKING

import pytest

from hermes_harness.git.worktrees import WorktreeError
from hermes_harness.multiagent.apply_edits import apply_new_files
from hermes_harness.multiagent.model_edits import parse_edit_proposal
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path


def _git(repo: Path, *args: str) -> str:
    binary = shutil.which('git')
    assert binary is not None
    result = subprocess.run(  # noqa: S603 - temporary test repository only
        [os.path.realpath(binary), '-C', str(repo), *args],
        capture_output=True,
        check=True,
        shell=False,
    )
    return result.stdout.decode().strip()


@pytest.fixture
def worktree(tmp_path: Path) -> tuple[Path, str]:
    repo = tmp_path / 'repo'
    repo.mkdir()
    _git(repo, 'init', '-q', '-b', 'main')
    _git(
        repo,
        '-c',
        'user.name=Test',
        '-c',
        'user.email=test@example.invalid',
        'commit',
        '--allow-empty',
        '-qm',
        'baseline',
    )
    workspace = tmp_path / 'workspace'
    _git(repo, 'worktree', 'add', '-b', 'test', str(workspace), 'HEAD')
    return workspace, _git(workspace, 'rev-parse', 'HEAD')


def _proposal() -> str:
    return '{"task_id":"a","files":[{"path":"tests/new.py","content":"assert True\\n"}]}'


def test_creates_only_scoped_new_file(worktree: tuple[Path, str]) -> None:
    workspace, baseline = worktree
    proposal = parse_edit_proposal(_proposal(), 'a', ('tests',))
    result = apply_new_files(workspace, proposal, ('tests',), baseline, authorized=True)
    assert result.audit.changed_paths == ('tests/new.py',)
    assert (workspace / 'tests/new.py').read_text() == 'assert True\n'


def test_requires_authorization(worktree: tuple[Path, str]) -> None:
    workspace, baseline = worktree
    proposal = parse_edit_proposal(_proposal(), 'a', ('tests',))
    with pytest.raises(WorkflowError, match='authorization'):
        apply_new_files(workspace, proposal, ('tests',), baseline)
    assert not (workspace / 'tests/new.py').exists()


def test_refuses_existing_content(worktree: tuple[Path, str]) -> None:
    workspace, baseline = worktree
    (workspace / 'tests').mkdir()
    (workspace / 'tests/new.py').write_text('existing')
    proposal = parse_edit_proposal(_proposal(), 'a', ('tests',))
    with pytest.raises(WorkflowError, match='clean worktree'):
        apply_new_files(workspace, proposal, ('tests',), baseline, authorized=True)
    assert (workspace / 'tests/new.py').read_text() == 'existing'


def test_refuses_symlink_ancestor(worktree: tuple[Path, str], tmp_path: Path) -> None:
    workspace, baseline = worktree
    (workspace / 'tests').symlink_to(tmp_path)
    proposal = parse_edit_proposal(_proposal(), 'a', ('tests',))
    with pytest.raises((WorkflowError, WorktreeError)):
        apply_new_files(workspace, proposal, ('tests',), baseline, authorized=True)
    assert not (tmp_path / 'new.py').exists()
