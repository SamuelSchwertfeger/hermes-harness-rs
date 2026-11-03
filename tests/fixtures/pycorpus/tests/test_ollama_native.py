"""Regression tests for Ollama-native structured inference transport."""

import io
import json
from typing import Any, Self

import pytest

from hermes_harness.multiagent.local_inference import (
    LocalEndpoint,
    LocalInferenceError,
    complete_text,
)
from hermes_harness.multiagent.ollama_native import (
    PROPOSAL_SCHEMA,
    REVIEW_SCHEMA,
    NativeOllamaJSONTransport,
)

EXPECTED_TIMEOUT = 30.0
EXPECTED_MAX_TOKENS = 512


class FakeResponse(io.BytesIO):
    status = 200

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *args: object) -> None:
        self.close()


class FakeOpener:
    def __init__(self, native_response: object) -> None:
        self.native_response = native_response
        self.payload: dict[str, Any] = {}
        self.url = ''

    def open(self, request: object, timeout: float) -> FakeResponse:
        assert timeout == EXPECTED_TIMEOUT
        self.url = request.full_url
        self.payload = json.loads(request.data)
        return FakeResponse(json.dumps(self.native_response).encode())


def _endpoint(url: str = 'http://127.0.0.1:11434/v1/chat/completions') -> LocalEndpoint:
    return LocalEndpoint(url=url, model='qwen3:4b', timeout_seconds=30, max_tokens=512)


def test_native_json_transport_disables_thinking_and_preserves_contract() -> None:
    opener = FakeOpener({'message': {'content': '{"verdict":"accept","findings":[]}'}})
    transport = NativeOllamaJSONTransport(
        schemas_by_model={'qwen3:4b': REVIEW_SCHEMA}, _opener=opener
    )
    text = complete_text(_endpoint(), 'Review code; output JSON.', 'def add(): return 2', transport)
    assert json.loads(text) == {'verdict': 'accept', 'findings': []}
    assert opener.url == 'http://127.0.0.1:11434/api/chat'
    assert opener.payload['think'] is False
    assert opener.payload['format'] == REVIEW_SCHEMA
    assert opener.payload['options']['num_predict'] == EXPECTED_MAX_TOKENS
    assert opener.payload['model'] == 'qwen3:4b'


def test_native_json_transport_rejects_empty_content() -> None:
    opener = FakeOpener({'message': {'content': '', 'thinking': 'reasoning only'}})
    with pytest.raises(LocalInferenceError, match='empty final content'):
        complete_text(
            _endpoint(),
            'system',
            'prompt',
            NativeOllamaJSONTransport(schemas_by_model={'qwen3:4b': REVIEW_SCHEMA}, _opener=opener),
        )


def test_native_json_transport_rejects_invalid_response() -> None:
    opener = FakeOpener({'unexpected': True})
    with pytest.raises(LocalInferenceError, match='Invalid Ollama native response'):
        complete_text(
            _endpoint(),
            'system',
            'prompt',
            NativeOllamaJSONTransport(schemas_by_model={'qwen3:4b': REVIEW_SCHEMA}, _opener=opener),
        )


def test_native_json_transport_keeps_loopback_guard() -> None:
    opener = FakeOpener({'message': {'content': '{}'}})
    endpoint = _endpoint('http://example.com/v1/chat/completions')
    with pytest.raises(LocalInferenceError, match='loopback'):
        complete_text(
            endpoint,
            'system',
            'prompt',
            NativeOllamaJSONTransport(schemas_by_model={'qwen3:4b': REVIEW_SCHEMA}, _opener=opener),
        )
    assert opener.url == ''


def test_native_json_transport_rejects_unmapped_model() -> None:
    opener = FakeOpener({'message': {'content': '{}'}})
    with pytest.raises(LocalInferenceError, match='operator-approved JSON schema'):
        complete_text(_endpoint(), 'system', 'prompt', NativeOllamaJSONTransport(_opener=opener))
    assert opener.url == ''


def test_proposal_and_review_schemas_are_distinct() -> None:
    assert set(PROPOSAL_SCHEMA['required']) == {'task_id', 'files'}
    assert set(REVIEW_SCHEMA['required']) == {'verdict', 'findings'}
    assert PROPOSAL_SCHEMA['additionalProperties'] is False
    assert REVIEW_SCHEMA['additionalProperties'] is False
