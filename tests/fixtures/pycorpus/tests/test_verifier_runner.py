"""Verifier allowlist and authorization tests avoid executing model code."""

import os
import shutil
import subprocess
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.multiagent.verification_evidence import evaluate_test_evidence
from hermes_harness.multiagent.verification_manifest import prepare_verification
from hermes_harness.multiagent.verifier_runner import run_verification, verify_and_evaluate

if TYPE_CHECKING:
    from pathlib import Path


def test_verifier_requires_authorization(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='authorization'):
        run_verification(tmp_path, 'a' * 40, ('src',), 'reviewer', 'pytest')


def test_verifier_rejects_unknown_command(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='Unknown verifier'):
        run_verification(
            tmp_path,
            'a' * 40,
            ('src',),
            'reviewer',
            'rm-all',
            authorized=True,
            trusted_code=True,
        )


def _git(repo: Path, *args: str) -> str:
    executable = shutil.which('git')
    assert executable is not None
    finished = subprocess.run(  # noqa: S603 - test-only isolated repository
        [os.path.realpath(executable), '-C', str(repo), *args],
        check=True,
        capture_output=True,
        shell=False,
    )
    return finished.stdout.decode().strip()


def test_allowlisted_pytest_binds_real_snapshot_evidence(tmp_path: Path) -> None:
    if shutil.which('uv') is None:
        pytest.skip('uv not installed in test environment')
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
    worktree = tmp_path / 'worktree'
    _git(repo, 'worktree', 'add', '-b', 'harness/verify', str(worktree), 'HEAD')
    baseline = _git(worktree, 'rev-parse', 'HEAD')
    (worktree / 'test_demo.py').write_text('def test_ok():\n    assert 1 + 1 == 2\n')
    manifest = prepare_verification(worktree, baseline, ('test_demo.py',))
    run = run_verification(
        worktree,
        baseline,
        ('test_demo.py',),
        'test-verifier',
        'pytest',
        authorized=True,
        trusted_code=True,
        timeout_seconds=45,
    )
    assert run.evidence.exit_code == 0, run.stderr
    assert evaluate_test_evidence(manifest, run.evidence).accepted
    assert not (repo / 'test_demo.py').exists()


def test_untrusted_code_never_executes_even_when_authorized(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='qualified isolated verifier'):
        run_verification(
            tmp_path,
            'a' * 40,
            ('src',),
            'verifier',
            'pytest',
            authorized=True,
            trusted_code=False,
        )


def test_wrapper_rejects_untrusted_execution(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='qualified isolated verifier'):
        verify_and_evaluate(
            tmp_path,
            'a' * 40,
            ('src',),
            'verifier',
            authorized=True,
            trusted_code=False,
        )
