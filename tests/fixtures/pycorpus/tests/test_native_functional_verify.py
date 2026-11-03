"""Sandbox verification authorization and non-execution regressions."""

# ruff: noqa: S603 - fixed local Git fixture setup, no shell

import hashlib
import json
import subprocess
from pathlib import Path

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native import functional_verify as gate


def _checkpoint(tmp_path: Path) -> tuple[Path, Path, Path]:
    workspace = tmp_path / 'worktree'
    workspace.mkdir()
    (workspace / 'tests').mkdir()
    (workspace / 'tests' / 'test_example.py').write_text('import unittest\n')
    subprocess.run(['/usr/bin/git', 'init', '-q', str(workspace)], check=True)
    subprocess.run(
        ['/usr/bin/git', '-C', str(workspace), 'add', 'tests/test_example.py'], check=True
    )
    subprocess.run(
        [
            '/usr/bin/git',
            '-C',
            str(workspace),
            '-c',
            'user.name=fixture',
            '-c',
            'user.email=fixture@example.invalid',
            'commit',
            '-qm',
            'baseline',
        ],
        check=True,
    )
    baseline = subprocess.check_output(
        ['/usr/bin/git', '-C', str(workspace), 'rev-parse', 'HEAD'], text=True
    ).strip()
    subprocess.run(
        ['/usr/bin/git', '-C', str(workspace), 'switch', '-qc', 'harness/test-one'], check=True
    )
    (workspace / 'reviewed.py').write_text('VALUE = 7\n')
    checkpoint = tmp_path / 'checkpoint.json'
    checkpoint.write_text(
        json.dumps({
            'mode': 'integrity-checkpoint',
            'integrity_verified': True,
            'functional_verified': False,
            'tests_executed': False,
            'human_decision_pending': True,
            'next_stage': 'requires-separate-functional-execution-authorization',
            'task_id': 'test-one',
            'baseline': baseline,
            'branch': 'harness/test-one',
            'workspace': str(workspace),
            'files': [
                {'path': 'reviewed.py', 'sha256': hashlib.sha256(b'VALUE = 7\n').hexdigest()}
            ],
        })
    )
    return workspace, checkpoint, tmp_path / 'result.json'


def test_rejects_without_separate_grant(tmp_path: Path) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    with pytest.raises(WorkflowError, match='authorization'):
        gate.functional_verify(checkpoint=checkpoint, workspace=workspace, evidence=evidence)
    assert not evidence.exists()


def test_rejects_missing_bwrap(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    monkeypatch.setattr(gate.shutil, 'which', lambda _: None)
    with pytest.raises(WorkflowError, match='Bubblewrap'):
        gate.functional_verify(
            checkpoint=checkpoint, workspace=workspace, evidence=evidence, authorized=True
        )
    assert not evidence.exists()


def test_rejects_tampered_reviewed_bytes(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    (workspace / 'reviewed.py').write_text('VALUE = 99\n')
    real_which = gate.shutil.which
    monkeypatch.setattr(
        gate.shutil, 'which', lambda name: '/usr/bin/bwrap' if name == 'bwrap' else real_which(name)
    )
    with pytest.raises(WorkflowError, match='changed since checkpoint'):
        gate.functional_verify(
            checkpoint=checkpoint, workspace=workspace, evidence=evidence, authorized=True
        )
    assert not evidence.exists()


def test_sandbox_command_has_no_shell_and_no_host_home(tmp_path: Path) -> None:
    command = gate._sandbox_argv(  # noqa: SLF001 - testing sandbox construction
        bwrap='/usr/bin/bwrap', workspace=tmp_path, python='/usr/bin/python3'
    )
    assert '--unshare-all' in command
    assert '--clearenv' in command
    assert '--ro-bind' in command
    assert '--die-with-parent' in command
    assert '-m' in command
    assert 'unittest' in command
    assert '/home' not in command
    assert 'sh' not in command


def test_success_records_bounded_evidence(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    real_which = gate.shutil.which
    monkeypatch.setattr(
        gate.shutil, 'which', lambda name: '/usr/bin/bwrap' if name == 'bwrap' else real_which(name)
    )
    monkeypatch.setattr(
        gate.Path,
        'is_file',
        lambda self: True if str(self) == '/usr/bin/python3' else Path.exists(self),
    )

    class Result:
        returncode = 0
        stdout = b'OK'
        stderr = b''

    def fake_run(*_args: object, **kwargs: object) -> Result:
        stderr = kwargs['stderr']
        stderr.write(b'Ran 1 test in 0.001s\n\nOK\n')
        return Result()

    gate._audit_checkout(workspace, gate._load(checkpoint)[0])  # noqa: SLF001
    monkeypatch.setattr(gate, '_audit_checkout', lambda *_args: {})
    monkeypatch.setattr(gate.subprocess, 'run', fake_run)
    output = gate.functional_verify(
        checkpoint=checkpoint, workspace=workspace, evidence=evidence, authorized=True
    )
    assert output['functional_verified'] is True
    assert output['next_stage'] == 'await-human-review'
    assert (evidence.stat().st_mode & 0o777) == 0o600  # noqa: PLR2004
    assert json.loads(evidence.read_text())['human_decision_pending'] is True


def test_no_test_directory_is_rejected(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    (workspace / 'tests' / 'test_example.py').unlink()
    (workspace / 'tests').rmdir()
    real_which = gate.shutil.which
    monkeypatch.setattr(
        gate.shutil, 'which', lambda name: '/usr/bin/bwrap' if name == 'bwrap' else real_which(name)
    )
    with pytest.raises(WorkflowError, match='tests directory'):
        gate.functional_verify(
            checkpoint=checkpoint, workspace=workspace, evidence=evidence, authorized=True
        )


@pytest.mark.parametrize(
    ('returncode', 'stderr_bytes', 'expected_executed', 'expected_stage'),
    [
        (
            1,
            b'Ran 1 test in 0.001s\n\nFAILED (failures=1)\n',
            True,
            'verification-failed-requires-review',
        ),
        (
            1,
            b'bwrap: Creating new namespace failed: Operation not permitted\n',
            False,
            'execution-unconfirmed-requires-review',
        ),
        (1, b'python3: failed to start\n', False, 'execution-unconfirmed-requires-review'),
    ],
)
def test_failing_tests_are_recorded_as_executed(  # noqa: PLR0913 - outcome matrix
    *,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    returncode: int,
    stderr_bytes: bytes,
    expected_executed: bool,
    expected_stage: str,
) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    real_which = gate.shutil.which
    monkeypatch.setattr(
        gate.shutil, 'which', lambda name: '/usr/bin/bwrap' if name == 'bwrap' else real_which(name)
    )
    monkeypatch.setattr(
        gate.Path,
        'is_file',
        lambda self: True if str(self) == '/usr/bin/python3' else Path.exists(self),
    )

    class Result:
        def __init__(self, rc: int) -> None:
            self.returncode = rc

    def fake_run(*_args: object, **kwargs: object) -> Result:
        kwargs['stderr'].write(stderr_bytes)
        return Result(returncode)

    gate._audit_checkout(workspace, gate._load(checkpoint)[0])  # noqa: SLF001
    monkeypatch.setattr(gate, '_audit_checkout', lambda *_args: {})
    monkeypatch.setattr(gate.subprocess, 'run', fake_run)
    outcome = gate.functional_verify(
        checkpoint=checkpoint, workspace=workspace, evidence=evidence, authorized=True
    )
    assert outcome['tests_executed'] is expected_executed
    assert outcome['generated_code_executed'] is expected_executed
    assert outcome['functional_verified'] is False
    assert outcome['next_stage'] == expected_stage
    assert json.loads(evidence.read_text())['tests_executed'] is expected_executed


def test_rejects_unreviewed_test_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    (workspace / 'tests' / 'test_injected.py').write_text('raise RuntimeError("unreviewed")\n')
    real_which = gate.shutil.which
    monkeypatch.setattr(
        gate.shutil, 'which', lambda name: '/usr/bin/bwrap' if name == 'bwrap' else real_which(name)
    )
    with pytest.raises(WorkflowError, match=r'audit|changed paths'):
        gate.functional_verify(
            checkpoint=checkpoint, workspace=workspace, evidence=evidence, authorized=True
        )
    assert not evidence.exists()


def test_rejects_changed_git_head(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    subprocess.run(
        [
            '/usr/bin/git',
            '-C',
            str(workspace),
            '-c',
            'user.name=fixture',
            '-c',
            'user.email=fixture@example.invalid',
            'commit',
            '-qm',
            'extra',
            '--allow-empty',
        ],
        check=True,
    )
    real_which = gate.shutil.which
    monkeypatch.setattr(
        gate.shutil, 'which', lambda name: '/usr/bin/bwrap' if name == 'bwrap' else real_which(name)
    )
    with pytest.raises(WorkflowError, match='identity changed'):
        gate.functional_verify(
            checkpoint=checkpoint, workspace=workspace, evidence=evidence, authorized=True
        )
    assert not evidence.exists()


def test_rejects_wrong_git_branch(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    subprocess.run(
        ['/usr/bin/git', '-C', str(workspace), 'switch', '-qc', 'other-branch'], check=True
    )
    real_which = gate.shutil.which
    monkeypatch.setattr(
        gate.shutil, 'which', lambda name: '/usr/bin/bwrap' if name == 'bwrap' else real_which(name)
    )
    with pytest.raises(WorkflowError, match='identity changed'):
        gate.functional_verify(
            checkpoint=checkpoint, workspace=workspace, evidence=evidence, authorized=True
        )
    assert not evidence.exists()


def test_rejects_ignored_test_module(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    workspace, checkpoint, evidence = _checkpoint(tmp_path)
    # Ignore rule is present in baseline for this synthetic security test.
    (workspace / '.git' / 'info' / 'exclude').write_text('tests/test_ignored.py\n')
    (workspace / 'tests' / 'test_ignored.py').write_text('raise RuntimeError("ignored")\n')
    real_which = gate.shutil.which
    monkeypatch.setattr(
        gate.shutil, 'which', lambda name: '/usr/bin/bwrap' if name == 'bwrap' else real_which(name)
    )
    with pytest.raises(WorkflowError, match='ignored Python test'):
        gate.functional_verify(
            checkpoint=checkpoint, workspace=workspace, evidence=evidence, authorized=True
        )
    assert not evidence.exists()
