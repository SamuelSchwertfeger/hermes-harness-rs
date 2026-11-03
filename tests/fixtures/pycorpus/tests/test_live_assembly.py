"""Live Hermes assembly admission and no-side-effect tests."""

import json
from typing import TYPE_CHECKING
from unittest.mock import patch

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.live_assembly import apply_live_assembly, live_assemble

if TYPE_CHECKING:
    from pathlib import Path


def test_live_requires_inference_grant(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='inference grant'):
        live_assemble(
            repo=tmp_path,
            plan=tmp_path / 'plan.json',
            state=tmp_path,
            registry=tmp_path / 'providers.json',
            session='local-qualification',
        )


def test_live_never_creates_repo_or_state_before_authorization(tmp_path: Path) -> None:
    target = tmp_path / 'missing'
    with pytest.raises(WorkflowError, match='inference grant'):
        live_assemble(
            repo=target,
            plan=tmp_path / 'plan.json',
            state=tmp_path,
            registry=tmp_path / 'providers.json',
            session='local-qualification',
        )
    assert not target.exists()


def test_apply_requires_independent_grant(tmp_path: Path) -> None:
    workspace = tmp_path / 'new-worktree'
    with pytest.raises(WorkflowError, match='application grant'):
        apply_live_assembly(
            repo=tmp_path,
            receipt=tmp_path / 'receipt.json',
            workspace=workspace,
        )
    assert not workspace.exists()


def test_apply_rejects_untrusted_receipt_before_worktree(tmp_path: Path) -> None:
    import subprocess  # noqa: PLC0415 - trusted disposable Git fixture

    repo = tmp_path / 'repo'
    repo.mkdir()
    subprocess.run(['/usr/bin/git', 'init', '-q', str(repo)], check=True)  # noqa: S603
    (repo / 'README.md').write_text('fixture')
    subprocess.run(['/usr/bin/git', '-C', str(repo), 'add', 'README.md'], check=True)  # noqa: S603
    subprocess.run(  # noqa: S603
        [
            '/usr/bin/git',
            '-C',
            str(repo),
            '-c',
            'user.name=Test',
            '-c',
            'user.email=test@example.invalid',
            'commit',
            '-qm',
            'initial',
        ],
        check=True,
    )
    receipt = tmp_path / 'receipt.json'
    receipt.write_text(json.dumps({'mode': 'untrusted', 'verdict': 'accept'}))
    worktree = tmp_path / 'new-worktree'
    with pytest.raises(WorkflowError, match='not eligible'):
        apply_live_assembly(
            repo=repo,
            receipt=receipt,
            workspace=worktree,
            authorized=True,
        )
    assert not worktree.exists()


def _invoke_xps(tmp_path: Path, reviewer: str | None) -> None:
    repo = tmp_path / 'repo'
    repo.mkdir()
    with (
        patch('hermes_harness.native.live_assembly.repository', return_value=repo),
        patch('hermes_harness.native.live_assembly._load_plan') as node,
        patch('hermes_harness.native.live_assembly._git', return_value=''),
        patch(
            'hermes_harness.native.live_assembly.available_models',
            return_value=('granite4:3b',),
        ),
    ):
        node.return_value.id = 'coding1'
        node.return_value.role = 'coding'
        live_assemble(
            repo=repo,
            plan=tmp_path / 'plan.json',
            state=tmp_path,
            registry=tmp_path / 'unused.json',
            session='test',
            authorize_inference=True,
            xps_worker_model='granite4:3b',
            xps_reviewer_model=reviewer,
        )


def test_xps_requires_worker_and_reviewer_pair(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='requires both worker'):
        _invoke_xps(tmp_path, None)
    assert not (tmp_path / 'coding1-live-assembly-claim.json').exists()


def test_xps_rejects_unknown_model_without_claim(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='not currently installed'):
        _invoke_xps(tmp_path, 'missing:7b')
    assert not (tmp_path / 'coding1-live-assembly-claim.json').exists()
