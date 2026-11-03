"""Actual Git worktree integrity verification, including fail-closed cases."""

import hashlib
import json
import subprocess
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.verify_applied import verify_applied

if TYPE_CHECKING:
    from pathlib import Path

CONTENT = 'def add(a: int, b: int) -> int:\n    return a + b'


def _git(root: Path, *args: str) -> str:
    result = subprocess.run(  # noqa: S603
        ['git', '-C', str(root), *args],  # noqa: S607 - controlled test Git invocation
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


@pytest.fixture
def case(tmp_path: Path) -> tuple[Path, Path, Path, Path]:
    root = tmp_path / 'source'
    root.mkdir()
    _git(root, 'init', '-q')
    _git(root, 'config', 'user.email', 'test@example.invalid')
    _git(root, 'config', 'user.name', 'Harness test')
    (root / 'base.txt').write_text('original\n')
    _git(root, 'add', 'base.txt')
    _git(root, 'commit', '-qm', 'baseline')
    head = _git(root, 'rev-parse', 'HEAD')
    worktree = tmp_path / 'worktree'
    _git(root, 'worktree', 'add', '-b', 'harness/test-audit', str(worktree), 'HEAD')
    (worktree / 'smoke_math.py').write_text(CONTENT)
    receipt = tmp_path / 'receipt.json'
    diff = (
        '--- /dev/null\n+++ b/smoke_math.py\n@@ -0,0 +1,2 @@\n'
        '+def add(a: int, b: int) -> int:\n+    return a + b'
    )
    receipt.write_text(
        json.dumps({
            'repository': str(root),
            'mode': 'live-reviewed-proposal-only',
            'verdict': 'accept',
            'applied': False,
            'head': head,
            'task_id': 'test-audit',
            'files': [
                {
                    'path': 'smoke_math.py',
                    'sha256': hashlib.sha256(CONTENT.encode()).hexdigest(),
                    'diff': diff,
                }
            ],
        })
    )
    evidence = tmp_path / 'verification.json'
    return root, worktree, receipt, evidence


def test_successful_integrity_audit(case: tuple[Path, Path, Path, Path]) -> None:
    root, worktree, receipt, evidence = case
    result = verify_applied(
        repo=root, receipt=receipt, workspace=worktree, evidence=evidence, authorized=True
    )
    assert result['integrity_verified'] is True
    assert result['changed_paths'] == ['smoke_math.py']
    assert result['generated_code_executed'] is False
    assert evidence.stat().st_mode & 0o777 == 0o600  # noqa: PLR2004 - required evidence permissions
    assert json.loads(evidence.read_text())['human_decision_pending'] is True


def test_requires_separate_grant(case: tuple[Path, Path, Path, Path]) -> None:
    root, worktree, receipt, evidence = case
    with pytest.raises(WorkflowError, match='authorization'):
        verify_applied(repo=root, receipt=receipt, workspace=worktree, evidence=evidence)
    assert not evidence.exists()


def test_rejects_modified_file(case: tuple[Path, Path, Path, Path]) -> None:
    root, worktree, receipt, evidence = case
    (worktree / 'smoke_math.py').write_text(CONTENT + '\n# changed')
    with pytest.raises(WorkflowError, match='content mismatch'):
        verify_applied(
            repo=root, receipt=receipt, workspace=worktree, evidence=evidence, authorized=True
        )
    assert not evidence.exists()


def test_rejects_extra_path(case: tuple[Path, Path, Path, Path]) -> None:
    root, worktree, receipt, evidence = case
    (worktree / 'other.py').write_text('extra')
    with pytest.raises(WorkflowError, match='outside ownership'):
        verify_applied(
            repo=root, receipt=receipt, workspace=worktree, evidence=evidence, authorized=True
        )
    assert not evidence.exists()


def test_rejects_reused_evidence(case: tuple[Path, Path, Path, Path]) -> None:
    root, worktree, receipt, evidence = case
    verify_applied(
        repo=root, receipt=receipt, workspace=worktree, evidence=evidence, authorized=True
    )
    with pytest.raises(WorkflowError, match='already exists'):
        verify_applied(
            repo=root, receipt=receipt, workspace=worktree, evidence=evidence, authorized=True
        )


def test_rejects_different_baseline(case: tuple[Path, Path, Path, Path]) -> None:
    root, worktree, receipt, evidence = case
    data = json.loads(receipt.read_text())
    data['head'] = 'a' * 40
    receipt.write_text(json.dumps(data))
    with pytest.raises(WorkflowError, match='baseline changed'):
        verify_applied(
            repo=root, receipt=receipt, workspace=worktree, evidence=evidence, authorized=True
        )
    assert not evidence.exists()
