"""Independent inference-only reviewer contract tests."""

import json
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.local_inference import LocalEndpoint, LocalInferenceError
from hermes_harness.multiagent.model_edits import parse_edit_proposal
from hermes_harness.multiagent.model_review import review_proposal


class ReviewTransport:
    def __init__(self, response: str) -> None:
        self.response = response

    def complete(self, endpoint: LocalEndpoint, payload: bytes) -> bytes:
        data = json.loads(payload)
        assert data['model'] == endpoint.model
        assert data['stream'] is False
        return json.dumps({'choices': [{'message': {'content': self.response}}]}).encode()


if TYPE_CHECKING:
    from hermes_harness.multiagent.model_edits import EditProposal


def _proposal() -> EditProposal:
    return parse_edit_proposal(
        json.dumps({
            'task_id': 't',
            'files': [{'path': 'src/a.py', 'content': 'value = 1\n'}],
        }),
        't',
        ('src',),
    )


def _endpoint(model: str) -> LocalEndpoint:
    return LocalEndpoint('http://127.0.0.1:8080/v1/chat/completions', model)


def test_read_only_review_verdict() -> None:
    result = review_proposal(
        _proposal(),
        _endpoint('worker'),
        _endpoint('reviewer'),
        ReviewTransport('{"verdict":"accept","findings":[]}'),
    )
    assert result.verdict == 'accept'
    assert result.authorizes_merge is False
    assert result.reviewer_model == 'reviewer'


def test_same_model_cannot_review_itself() -> None:
    with pytest.raises(LocalInferenceError, match='distinct'):
        review_proposal(
            _proposal(),
            _endpoint('same'),
            _endpoint('same'),
            ReviewTransport('{"verdict":"accept","findings":[]}'),
        )


def test_rejects_unsupported_verdict() -> None:
    with pytest.raises(LocalInferenceError, match='Unsupported'):
        review_proposal(
            _proposal(),
            _endpoint('worker'),
            _endpoint('reviewer'),
            ReviewTransport('{"verdict":"approve_merge","findings":[]}'),
        )


def test_reviewer_receives_exact_task_scope() -> None:
    class CaptureReview(ReviewTransport):
        def complete(self, endpoint: LocalEndpoint, payload: bytes) -> bytes:
            messages = json.loads(payload)['messages']
            review_input = json.loads(messages[1]['content'])
            assert review_input['objective'] == 'Implement only add(a, b) as a + b'
            assert 'unrelated' in messages[0]['content']
            return super().complete(endpoint, payload)

    result = review_proposal(
        _proposal(),
        _endpoint('worker'),
        _endpoint('reviewer'),
        CaptureReview('{"verdict":"accept","findings":[]}'),
        objective='Implement only add(a, b) as a + b',
    )
    assert result.verdict == 'accept'
