"""Local worker/reviewer orchestration is serial and approval-gated."""

import json
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.local_inference import LocalEndpoint
from hermes_harness.multiagent.model_pipeline import (
    apply_reviewed_new_files,
    generate_and_review,
)
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path


class SerialResponses:
    def __init__(self, verdict: str = 'accept') -> None:
        self.calls: list[str] = []
        self.verdict = verdict

    def complete(self, endpoint: LocalEndpoint, _payload: bytes) -> bytes:
        self.calls.append(endpoint.model)
        if endpoint.model == 'coder':
            text = json.dumps({
                'task_id': 'task-1',
                'files': [{'path': 'tests/new.py', 'content': 'assert True\n'}],
            })
        else:
            text = json.dumps({'verdict': self.verdict, 'findings': []})
        return json.dumps({'choices': [{'message': {'content': text}}]}).encode()


def _endpoint(model: str) -> LocalEndpoint:
    return LocalEndpoint('http://127.0.0.1:11434/v1/chat/completions', model)


def test_serial_model_proposal_and_review() -> None:
    transport = SerialResponses()
    outcome = generate_and_review(
        'task-1',
        'Write test',
        ('tests',),
        _endpoint('coder'),
        _endpoint('reviewer'),
        transport,
        inference_authorized=True,
    )
    assert transport.calls == ['coder', 'reviewer']
    assert outcome.review.verdict == 'accept'
    assert outcome.applied is None
    assert outcome.review.authorizes_merge is False


def test_inference_requires_authorization() -> None:
    transport = SerialResponses()
    with pytest.raises(WorkflowError, match='authorization'):
        generate_and_review(
            'task-1',
            'Write test',
            ('tests',),
            _endpoint('coder'),
            _endpoint('reviewer'),
            transport,
        )
    assert transport.calls == []


def test_refused_review_cannot_be_applied(tmp_path: Path) -> None:
    outcome = generate_and_review(
        'task-1',
        'Write test',
        ('tests',),
        _endpoint('coder'),
        _endpoint('reviewer'),
        SerialResponses('changes_required'),
        inference_authorized=True,
    )
    with pytest.raises(WorkflowError, match='did not accept'):
        apply_reviewed_new_files(
            outcome, tmp_path, ('tests',), 'unused', application_authorized=True
        )
    assert not (tmp_path / 'tests').exists()


def test_application_needs_independent_grant(tmp_path: Path) -> None:
    outcome = generate_and_review(
        'task-1',
        'Write test',
        ('tests',),
        _endpoint('coder'),
        _endpoint('reviewer'),
        SerialResponses(),
        inference_authorized=True,
    )
    with pytest.raises(WorkflowError, match='application'):
        apply_reviewed_new_files(outcome, tmp_path, ('tests',), 'unused')
