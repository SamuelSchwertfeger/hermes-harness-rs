"""Human decision artifacts remain unapproved, immutable, and receipt-bound."""

import json
from typing import TYPE_CHECKING
from unittest.mock import patch

import pytest

from hermes_harness.agents.supervised_review_handoff import ReviewerHandoff
from hermes_harness.supervised.live_human_decision import (
    DecisionEvidence,
    DecisionRequest,
    HumanDecisionError,
    request_human_decision_once,
)

if TYPE_CHECKING:
    from pathlib import Path


class FakeReceipt:
    def __init__(self, submission_id: str, admission_id: str, response: str) -> None:
        self.submission_id = submission_id
        self.admission_id = admission_id
        self.outcome = 'completed'
        self.final_response = response


def evidence() -> DecisionEvidence:
    response = json.dumps({
        'task_id': 'task',
        'worker_submission_id': 'worker',
        'audit_sha256': 'a' * 64,
        'decision': 'changes_requested',
        'findings': ['Add a regression test'],
    })
    return DecisionEvidence(
        worker=FakeReceipt('worker', 'worker-adm', 'patched code'),  # type: ignore[arg-type]
        reviewer=FakeReceipt('reviewer', 'reviewer-adm', response),  # type: ignore[arg-type]
    )


def handoff() -> ReviewerHandoff:
    return ReviewerHandoff(
        task_id='task',
        reviewer_id='reviewer-agent',
        worker_submission_id='worker',
        worker_admission_id='worker-adm',
        audit_sha256='a' * 64,
        reviewer_prompt='Audit this worktree',
    )


def test_request_written_once_without_approval(tmp_path: Path) -> None:
    with patch('hermes_harness.supervised.live_human_decision.verify_reviewer_handoff') as verify:
        path = request_human_decision_once(
            DecisionRequest(task=None, handoff=handoff(), receipts=evidence(), output_dir=tmp_path)  # type: ignore[arg-type]
        )
        verify.assert_called_once()
        document = json.loads(path.read_text())
        assert document['approved'] is False
        assert document['human_decision'] == 'pending'
        assert document['reviewer_verdict']['decision'] == 'changes_requested'
        with pytest.raises(HumanDecisionError, match='already exists'):
            request_human_decision_once(
                DecisionRequest(
                    task=None, handoff=handoff(), receipts=evidence(), output_dir=tmp_path
                )  # type: ignore[arg-type]
            )


def test_reviewer_identity_collision_rejected(tmp_path: Path) -> None:
    receipts = evidence()
    receipts.reviewer.submission_id = 'worker'  # type: ignore[attr-defined]
    with (
        patch('hermes_harness.supervised.live_human_decision.verify_reviewer_handoff'),
        pytest.raises(HumanDecisionError, match='must differ'),
    ):
        request_human_decision_once(
            DecisionRequest(task=None, handoff=handoff(), receipts=receipts, output_dir=tmp_path)  # type: ignore[arg-type]
        )
    assert not list(tmp_path.iterdir())


def test_handoff_verifier_failure_aborts_before_write(tmp_path: Path) -> None:
    with (
        patch(
            'hermes_harness.supervised.live_human_decision.verify_reviewer_handoff',
            side_effect=ValueError('tamper'),
        ),
        pytest.raises(ValueError, match='tamper'),
    ):
        request_human_decision_once(
            DecisionRequest(task=None, handoff=handoff(), receipts=evidence(), output_dir=tmp_path)  # type: ignore[arg-type]
        )
    assert not list(tmp_path.iterdir())
