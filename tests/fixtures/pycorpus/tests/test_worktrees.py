"""Tests for isolated worktrees and enforceable Git path ownership."""

import shutil
import subprocess
from pathlib import Path  # noqa: TC003 - Python 3.13 runtime annotation

import pytest

from hermes_harness.git.worktrees import (
    WorktreeError,
    WorktreeSpec,
    audit_worktree,
    create_worktree,
    remove_worktree,
)


def git(repo: Path, *args: str) -> str:
    git_executable = shutil.which('git')
    if git_executable is None:
        pytest.skip('git is unavailable')
    result = subprocess.run(  # noqa: S603
        [git_executable, '-C', str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    source = tmp_path / 'source'
    source.mkdir()
    git(source, 'init', '-b', 'main')
    git(source, 'config', 'user.email', 'tests@example.invalid')
    git(source, 'config', 'user.name', 'Harness Test')
    (source / 'src').mkdir()
    (source / 'src' / 'app.py').write_text('original\n')
    git(source, 'add', '.')
    git(source, 'commit', '-m', 'Initial')
    return source


def test_isolation_and_audit(repo: Path) -> None:
    workspace = repo.parent / 'ticket-a'
    spec = WorktreeSpec('ticket-a', 'harness/ticket-a', ('src/app.py',))
    create_worktree(repo, workspace, spec)
    baseline = git(workspace, 'rev-parse', 'HEAD')
    (workspace / 'src' / 'app.py').write_text('changed\n')
    audit = audit_worktree(workspace, baseline, spec.owned_paths)
    assert audit.changed_paths == ('src/app.py',)
    assert (repo / 'src' / 'app.py').read_text() == 'original\n'
    with pytest.raises(WorktreeError, match='dirty'):
        remove_worktree(repo, workspace, expected_branch=spec.branch)
    (workspace / 'src' / 'app.py').write_text('original\n')
    remove_worktree(repo, workspace, expected_branch=spec.branch)


def test_unowned_and_untracked_rejected(repo: Path) -> None:
    workspace = repo.parent / 'ticket-b'
    spec = WorktreeSpec('ticket-b', 'harness/ticket-b', ('src',))
    create_worktree(repo, workspace, spec)
    baseline = git(workspace, 'rev-parse', 'HEAD')
    (workspace / 'secret.txt').write_text('untracked\n')
    with pytest.raises(WorktreeError, match='outside ownership'):
        audit_worktree(workspace, baseline, spec.owned_paths)


def test_symlink_and_existing_workspace_rejected(repo: Path) -> None:
    workspace = repo.parent / 'ticket-c'
    spec = WorktreeSpec('ticket-c', 'harness/ticket-c', ('src',))
    create_worktree(repo, workspace, spec)
    with pytest.raises(WorktreeError, match='already exists'):
        create_worktree(repo, workspace, spec)
    baseline = git(workspace, 'rev-parse', 'HEAD')
    (workspace / 'src' / 'link').symlink_to('/etc/passwd')
    with pytest.raises(WorktreeError, match='escapes worktree'):
        audit_worktree(workspace, baseline, spec.owned_paths)


def test_bad_ticket_and_ownership(repo: Path) -> None:
    with pytest.raises(WorktreeError, match='Invalid ticket'):
        create_worktree(
            repo, repo.parent / 'bad', WorktreeSpec('../bad', 'harness/../bad', ('src',))
        )
    with pytest.raises(WorktreeError, match='Invalid relative path'):
        create_worktree(
            repo, repo.parent / 'bad', WorktreeSpec('ok', 'harness/ok', ('../outside',))
        )
