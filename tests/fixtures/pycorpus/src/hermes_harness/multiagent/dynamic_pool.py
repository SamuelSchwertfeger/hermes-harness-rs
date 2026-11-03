"""Operator-approved on-demand inference from any installed XPS Ollama model."""

import json
import re

from hermes_harness.multiagent.local_inference import (
    LocalEndpoint,
    LocalInferenceError,
    complete_text,
)
from hermes_harness.multiagent.ollama_discovery import ollama_models
from hermes_harness.multiagent.ollama_native import NativeOllamaJSONTransport

XPS_BASE = 'http://127.0.0.1:11435'
MODEL_ID = re.compile(r'[a-zA-Z0-9_][a-zA-Z0-9_.:/-]{0,127}\Z')
RESPONSE_SCHEMA: dict[str, object] = {
    'type': 'object',
    'properties': {'response': {'type': 'string'}},
    'required': ['response'],
    'additionalProperties': False,
}


def available_models(base_url: str = XPS_BASE) -> tuple[str, ...]:
    """Discover installed models live, with no model downloads."""
    if base_url != XPS_BASE:
        msg = 'Unapproved XPS inference endpoint'
        raise LocalInferenceError(msg)
    return tuple(
        model.name
        for model in ollama_models(base_url)
        if MODEL_ID.fullmatch(model.name)
        and all(part not in ('', '.', '..') for part in model.name.split('/'))
    )


def infer(
    model: str,
    prompt: str,
    *,
    authorized: bool = False,
) -> dict[str, object]:
    """Infer using any currently installed model; never run model-issued commands."""
    if not authorized:
        msg = 'XPS inference needs operator authorization'
        raise LocalInferenceError(msg)
    if model not in available_models():
        msg = 'Requested XPS model unavailable'
        raise LocalInferenceError(msg)
    endpoint = LocalEndpoint(
        f'{XPS_BASE}/v1/chat/completions',
        model,
        timeout_seconds=120,
        max_tokens=512,
    )
    transport = NativeOllamaJSONTransport(schemas_by_model={model: RESPONSE_SCHEMA})
    result = complete_text(
        endpoint,
        'Return one JSON object containing response as a string. '
        'Do not claim tool execution or file edits.',
        prompt,
        transport,
    )
    try:
        parsed: object = json.loads(result)
    except ValueError as exc:
        msg = 'Invalid XPS response JSON'
        raise LocalInferenceError(msg) from exc
    if (
        not isinstance(parsed, dict)
        or set(parsed) != {'response'}
        or not isinstance(parsed['response'], str)
    ):
        msg = 'Invalid XPS response schema'
        raise LocalInferenceError(msg)
    return {'model': model, 'response': parsed['response'], 'executed_tools': False}
