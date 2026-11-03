"""Deterministic delegation tests; no models, network or Git writes."""

from dataclasses import replace
from typing import TYPE_CHECKING

import pytest

from hermes_harness.core.delegation import (
    Brief,
    DelegationError,
    Finding,
    Phase,
    begin,
    delegate,
    phase,
    request_approval,
    review,
)

if TYPE_CHECKING:
    from pathlib import Path


class FakeAgent:
    def __init__(self, identity: str) -> None:
        self.identity = identity
        self.calls = 0

    def run(self, _brief: Brief, prior: Finding | None) -> Finding:
        self.calls += 1
        return Finding(self.identity, 'Reviewed worker' if prior else 'Worker output')


@pytest.fixture
def brief() -> Brief:
    return Brief('task-one', 'Offline example', ('src/safe.py',), 'worker', 'reviewer')


def test_complete_offline_handoff(tmp_path: Path, brief: Brief) -> None:
    begin(tmp_path, brief)
    assert phase(tmp_path, brief) is Phase.CREATED
    delegate(tmp_path, brief, FakeAgent('worker'))
    assert phase(tmp_path, brief) is Phase.WORKER_DONE
    review(tmp_path, brief, FakeAgent('reviewer'))
    assert phase(tmp_path, brief) is Phase.REVIEWED
    path = request_approval(tmp_path, brief)
    assert phase(tmp_path, brief) is Phase.AWAITING_APPROVAL
    assert b'"approved":false' in path.read_bytes()


def test_recovery_prevents_worker_reexecution(tmp_path: Path, brief: Brief) -> None:
    begin(tmp_path, brief)
    worker = FakeAgent('worker')
    delegate(tmp_path, brief, worker)
    with pytest.raises(DelegationError):
        delegate(tmp_path, brief, worker)
    assert worker.calls == 1


def test_review_requires_worker(tmp_path: Path, brief: Brief) -> None:
    begin(tmp_path, brief)
    with pytest.raises(DelegationError):
        review(tmp_path, brief, FakeAgent('reviewer'))


def test_reviewer_must_be_independent(tmp_path: Path, brief: Brief) -> None:
    with pytest.raises(DelegationError):
        begin(tmp_path, replace(brief, reviewer_id='worker'))


def test_wrong_identity_not_persisted(tmp_path: Path, brief: Brief) -> None:
    begin(tmp_path, brief)
    with pytest.raises(DelegationError):
        delegate(tmp_path, brief, FakeAgent('intruder'))
    assert phase(tmp_path, brief) is Phase.CREATED


def test_mutated_brief_rejected(tmp_path: Path, brief: Brief) -> None:
    begin(tmp_path, brief)
    with pytest.raises(DelegationError):
        phase(tmp_path, replace(brief, objective='Modified'))


def test_approval_requires_review(tmp_path: Path, brief: Brief) -> None:
    begin(tmp_path, brief)
    delegate(tmp_path, brief, FakeAgent('worker'))
    with pytest.raises(DelegationError):
        request_approval(tmp_path, brief)


def test_bad_ownership_rejected(tmp_path: Path, brief: Brief) -> None:
    with pytest.raises(DelegationError):
        begin(tmp_path, replace(brief, owned_paths=('../secrets',)))
