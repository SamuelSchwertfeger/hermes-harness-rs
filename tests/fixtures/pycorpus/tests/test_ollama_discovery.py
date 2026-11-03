"""Ollama inventory parsing does not contact a server in these tests."""

import json

import pytest

from hermes_harness.multiagent.local_inference import LocalInferenceError
from hermes_harness.multiagent.ollama_discovery import parse_ollama_tags

EXPECTED_MODEL_SIZE = 123


def test_parses_ollama_models() -> None:
    models = parse_ollama_tags(
        json.dumps({'models': [{'name': 'local-coder:latest', 'size': 123}]}).encode()
    )
    assert models[0].name == 'local-coder:latest'
    assert models[0].size_bytes == EXPECTED_MODEL_SIZE


def test_handles_empty_inventory() -> None:
    assert parse_ollama_tags(b'{"models":[]}') == ()


@pytest.mark.parametrize(
    'value',
    [
        b'{}',
        b'{invalid',
        b'{"models":[{"name":"model","size":true}]}',
        b'{"models":[{"name":"","size":1}]}',
    ],
)
def test_rejects_invalid_inventory(value: bytes) -> None:
    with pytest.raises(LocalInferenceError):
        parse_ollama_tags(value)
