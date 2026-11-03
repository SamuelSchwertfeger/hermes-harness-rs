"""Offline deterministic transport tests for the local model adapter."""

import hashlib
import json

import pytest

from hermes_harness.multiagent.local_inference import (
    LocalEndpoint,
    LocalInferenceError,
    complete_text,
    propose_edits,
    propose_plan,
    propose_replacements,
)


class FakeTransport:
    def __init__(self, text: str) -> None:
        self.text = text

    def complete(self, endpoint: LocalEndpoint, payload: bytes) -> bytes:
        data = json.loads(payload)
        assert data['model'] == endpoint.model
        assert data['stream'] is False
        return json.dumps({'choices': [{'message': {'content': self.text}}]}).encode()


def test_accepts_structured_local_plan() -> None:
    plan = json.dumps({
        'id': 'ticket',
        'objective': 'Add test',
        'nodes': [{'id': 'test', 'objective': 'Write a test', 'owned_paths': ['tests/x.py']}],
    })
    output = propose_plan(
        LocalEndpoint('http://127.0.0.1:8080/v1/chat/completions', 'configured-model'),
        'Create a test',
        FakeTransport(plan),
    )
    assert output.nodes[0].id == 'test'


@pytest.mark.parametrize(
    'url',
    [
        'https://127.0.0.1/v1/chat/completions',
        'http://example.com/v1/chat/completions',
        'http://localhost/v1/chat/completions',
        'http://127.0.0.1:8080/other',
        'http://127.0.0.1:8080/v1/chat/completions?token=x',
    ],
)
def test_rejects_unqualified_endpoint(url: str) -> None:
    with pytest.raises(LocalInferenceError, match='loopback'):
        LocalEndpoint(url, 'test').validate()


def test_rejects_unstructured_response() -> None:
    endpoint = LocalEndpoint('http://127.0.0.1:8080/v1/chat/completions', 'test')
    with pytest.raises(LocalInferenceError, match='plan admission'):
        propose_plan(endpoint, 'create plan', FakeTransport('not json'))


def test_budget_and_response_contract() -> None:
    endpoint = LocalEndpoint('http://127.0.0.1:8080/v1/chat/completions', 'test')
    assert complete_text(endpoint, 'system', 'prompt', FakeTransport('ok')) == 'ok'
    with pytest.raises(LocalInferenceError, match='oversized'):
        complete_text(endpoint, 'system', 'x' * 20_000, FakeTransport('ok'))
    with pytest.raises(LocalInferenceError, match='budget'):
        LocalEndpoint(endpoint.url, 'test', max_tokens=10_000).validate()


def test_local_model_returns_scoped_file_proposal() -> None:
    endpoint = LocalEndpoint('http://127.0.0.1:8080/v1/chat/completions', 'test')
    body = json.dumps({
        'task_id': 'ticket-a',
        'files': [{'path': 'tests/new.py', 'content': 'assert True\\n'}],
    })
    result = propose_edits(endpoint, 'ticket-a', 'Add test', ('tests',), FakeTransport(body))
    assert result.files[0].path == 'tests/new.py'


def test_local_model_unowned_proposal_rejected() -> None:
    endpoint = LocalEndpoint('http://127.0.0.1:8080/v1/chat/completions', 'test')
    body = json.dumps({
        'task_id': 'ticket-a',
        'files': [{'path': 'outside.py', 'content': 'bad'}],
    })
    with pytest.raises(LocalInferenceError, match='ownership admission'):
        propose_edits(endpoint, 'ticket-a', 'Add test', ('tests',), FakeTransport(body))


def test_local_replacement_echoes_supplied_preimage() -> None:

    endpoint = LocalEndpoint('http://127.0.0.1:11434/v1/chat/completions', 'coder')
    digest = hashlib.sha256(b'original').hexdigest()
    response = json.dumps({
        'task_id': 'task-a',
        'replacements': [
            {
                'path': 'src/code.py',
                'expected_sha256': digest,
                'content': 'updated = True\\n',
            }
        ],
    })
    result = propose_replacements(
        endpoint,
        'task-a',
        'Update code',
        ('src',),
        {'src/code.py': digest},
        FakeTransport(response),
    )
    assert result[0].expected_sha256 == digest


def test_local_replacement_rejects_different_preimage() -> None:
    endpoint = LocalEndpoint('http://127.0.0.1:11434/v1/chat/completions', 'coder')
    response = json.dumps({
        'task_id': 'task-a',
        'replacements': [
            {
                'path': 'src/code.py',
                'expected_sha256': 'b' * 64,
                'content': 'unsafe',
            }
        ],
    })
    with pytest.raises(LocalInferenceError, match='does not match'):
        propose_replacements(
            endpoint,
            'task-a',
            'Update code',
            ('src',),
            {'src/code.py': 'a' * 64},
            FakeTransport(response),
        )
