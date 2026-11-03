"""No live gateway: test the irreversible reviewer dispatch boundary with doubles."""

import asyncio
from dataclasses import replace
from typing import TYPE_CHECKING

import pytest

from hermes_harness.agents.supervised_review_handoff import ReviewerHandoff
from hermes_harness.agents.supervised_reviewer_dispatch import (
    DisabledRuntimeQualifier,
    ReviewerDispatchBlockedError,
    ReviewerDispatchInputs,
    submit_reviewer_once,
)
from hermes_harness.core.delegation import Brief
from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_identity import GatewayOutcomeUnknownError, Submission
from hermes_harness.git.worktree_delegation import TaskWorkspace

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.agents.supervised_reviewer_dispatch import RuntimeBinding


class AllowForScriptedTest:
    def __init__(self) -> None:
        self.calls: list[RuntimeBinding] = []

    def require_qualified(self, binding: RuntimeBinding) -> None:
        self.calls.append(binding)


class ScriptedRPC:
    def __init__(self, *, fail: bool = False) -> None:
        self.calls: list[str] = []
        self.fail = fail

    async def rpc(self, method: str, **_params: object) -> object:
        self.calls.append(method)
        if self.fail and method == 'prompt.submit':
            msg = 'unconfirmed transport failure'
            raise OSError(msg)
        return {'admission_id': 'review-admission', 'status': 'queued'}


def _inputs(tmp_path: Path) -> tuple[ReviewerDispatchInputs, ScriptedRPC, AllowForScriptedTest]:
    # This test checks the gateway boundary without requiring Git fixtures.

    workspace = tmp_path / 'worktree'
    workspace.mkdir()
    task = TaskWorkspace(
        tmp_path / 'state', workspace, Brief('task', 'demo', ('src/a.py',), 'worker', 'reviewer')
    )
    handoff = ReviewerHandoff(
        task_id='task',
        reviewer_id='reviewer',
        worker_submission_id='worker-sub',
        worker_admission_id='worker-admission',
        audit_sha256='abc',
        reviewer_prompt='Review evidence',
    )
    rpc = ScriptedRPC()
    qualifier = AllowForScriptedTest()
    inputs = ReviewerDispatchInputs(
        task=task,
        handoff=handoff,
        submission=Submission('review-session', 'review-sub'),
        store=AdmissionStore(tmp_path / 'ledger.sqlite'),
        rpc=rpc,
        qualifier=qualifier,
    )
    return inputs, rpc, qualifier


def test_disabled_gate_prevents_submission(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    inputs, rpc, _ = _inputs(tmp_path)
    monkeypatch.setattr(
        'hermes_harness.agents.supervised_reviewer_dispatch.verify_reviewer_handoff',
        lambda *_: None,
    )

    inputs = replace(inputs, qualifier=DisabledRuntimeQualifier())
    with pytest.raises(ReviewerDispatchBlockedError, match='disabled'):
        asyncio.run(submit_reviewer_once(inputs))
    assert rpc.calls == []
    assert inputs.store.get('review-sub') is None


def test_qualified_scripted_submission_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    inputs, rpc, qualifier = _inputs(tmp_path)
    monkeypatch.setattr(
        'hermes_harness.agents.supervised_reviewer_dispatch.verify_reviewer_handoff',
        lambda *_: None,
    )
    result = asyncio.run(submit_reviewer_once(inputs))
    assert result.admission_id == 'review-admission'
    assert rpc.calls == ['prompt.submit', 'prompt.receipt']
    assert qualifier.calls[0].session_id == 'review-session'
    with pytest.raises(ReviewerDispatchBlockedError, match='already recorded'):
        asyncio.run(submit_reviewer_once(inputs))
    assert rpc.calls == ['prompt.submit', 'prompt.receipt']


def test_uncertain_submit_never_retries(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    inputs, rpc, _ = _inputs(tmp_path)
    rpc.fail = True
    monkeypatch.setattr(
        'hermes_harness.agents.supervised_reviewer_dispatch.verify_reviewer_handoff',
        lambda *_: None,
    )
    with pytest.raises(GatewayOutcomeUnknownError):
        asyncio.run(submit_reviewer_once(inputs))
    record = inputs.store.get('review-sub')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    with pytest.raises(ReviewerDispatchBlockedError):
        asyncio.run(submit_reviewer_once(inputs))
    assert rpc.calls == ['prompt.submit']


def test_rejected_stale_audit_never_calls_qualifier(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    inputs, rpc, qualifier = _inputs(tmp_path)

    def reject(*_args: object) -> None:
        msg = 'Audit mismatch'
        raise RuntimeError(msg)

    monkeypatch.setattr(
        'hermes_harness.agents.supervised_reviewer_dispatch.verify_reviewer_handoff', reject
    )
    with pytest.raises(RuntimeError, match='Audit mismatch'):
        asyncio.run(submit_reviewer_once(inputs))
    assert not qualifier.calls
    assert not rpc.calls
