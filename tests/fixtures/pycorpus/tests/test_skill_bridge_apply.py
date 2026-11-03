"""The Hermes bridge must require an independent human application grant."""

import argparse
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.skill_bridge import _parser, handle

if TYPE_CHECKING:
    from pathlib import Path


def test_apply_parser_does_not_implicitly_authorize() -> None:
    args = _parser().parse_args([
        'apply-reviewed',
        '--repo',
        '/tmp/source',  # noqa: S108
        '--receipt',
        '/tmp/receipt.json',  # noqa: S108
        '--workspace',
        '/tmp/worktree',  # noqa: S108
        '--session',
        'session-1',
    ])
    assert args.authorize_apply is False


def test_apply_bridge_denies_without_grant(tmp_path: Path) -> None:
    args = argparse.Namespace(
        command='apply-reviewed',
        repo=tmp_path,
        receipt=tmp_path / 'receipt.json',
        workspace=tmp_path / 'worktree',
        session='session-1',
        authorize_apply=False,
    )
    with pytest.raises(WorkflowError, match='authorization'):
        handle(args)


def test_apply_bridge_delegates_with_explicit_grant(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from hermes_harness.native import worktree_apply  # noqa: PLC0415

    calls: list[dict[str, object]] = []

    def fake_apply(  # noqa: PLR0913
        *, repo: Path, receipt: Path, workspace: Path, authorized: bool = False
    ) -> dict[str, object]:
        calls.append({
            'repo': repo,
            'receipt': receipt,
            'workspace': workspace,
            'authorized': authorized,
        })
        return {'mode': 'authorized-disposable-worktree', 'task_id': 'test-1'}

    monkeypatch.setattr(worktree_apply, 'apply_reviewed_receipt', fake_apply)
    args = argparse.Namespace(
        command='apply-reviewed',
        repo=tmp_path,
        receipt=tmp_path / 'receipt.json',
        workspace=tmp_path / 'worktree',
        session='session-1',
        authorize_apply=True,
    )
    response = handle(args)
    assert calls[0]['authorized'] is True
    assert response['session_id'] == 'session-1'
    assert response['task_id'] == 'test-1'
