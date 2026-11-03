"""Content-addressed existing-file replacement in a temporary worktree."""

import hashlib
import os
import shutil
import subprocess
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.replace_edits import FileReplacement, apply_replacements
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path


def _git(repo: Path, *args: str) -> str:
    binary = shutil.which('git')
    assert binary is not None
    result = subprocess.run(  # noqa: S603 - isolated Git test fixture
        [os.path.realpath(binary), '-C', str(repo), *args],
        check=True,
        capture_output=True,
        shell=False,
    )
    return result.stdout.decode().strip()


@pytest.fixture
def checkout(tmp_path: Path) -> tuple[Path, str]:
    repo = tmp_path / 'repo'
    repo.mkdir()
    _git(repo, 'init', '-q', '-b', 'main')
    (repo / 'value.py').write_text('old = 1\n')
    _git(repo, 'add', 'value.py')
    _git(
        repo,
        '-c',
        'user.name=Test',
        '-c',
        'user.email=test@example.invalid',
        'commit',
        '-qm',
        'baseline',
    )
    workspace = tmp_path / 'workspace'
    _git(repo, 'worktree', 'add', '-b', 'harness/change', str(workspace), 'HEAD')
    return workspace, _git(workspace, 'rev-parse', 'HEAD')


def test_replaces_only_matching_preimage(checkout: tuple[Path, str]) -> None:
    workspace, baseline = checkout
    digest = hashlib.sha256(b'old = 1\n').hexdigest()
    audit = apply_replacements(
        workspace,
        (FileReplacement('value.py', digest, 'new = 2\n'),),
        ('value.py',),
        baseline,
        authorized=True,
    )
    assert audit.changed_paths == ('value.py',)
    assert (workspace / 'value.py').read_text() == 'new = 2\n'


def test_rejects_stale_hash(checkout: tuple[Path, str]) -> None:
    workspace, baseline = checkout
    with pytest.raises(WorkflowError, match='preimage'):
        apply_replacements(
            workspace,
            (FileReplacement('value.py', 'a' * 64, 'no'),),
            ('value.py',),
            baseline,
            authorized=True,
        )
    assert (workspace / 'value.py').read_text() == 'old = 1\n'


def test_requires_explicit_grant(checkout: tuple[Path, str]) -> None:
    workspace, baseline = checkout
    with pytest.raises(WorkflowError, match='authorization'):
        apply_replacements(
            workspace,
            (FileReplacement('value.py', 'a' * 64, 'no'),),
            ('value.py',),
            baseline,
        )
