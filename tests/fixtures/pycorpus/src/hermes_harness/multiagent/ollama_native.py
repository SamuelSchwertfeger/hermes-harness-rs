"""Bounded Ollama-native structured JSON adapter for the local inference protocol."""

import json
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, cast
from urllib.parse import urlsplit

from hermes_harness.multiagent.local_inference import (
    HTTP_OK,
    MAX_RESPONSE_BYTES,
    LocalInferenceError,
)

if TYPE_CHECKING:
    from collections.abc import Mapping

    from hermes_harness.multiagent.local_inference import LocalEndpoint


# The model-specific wire format is constrained separately from the downstream
# ownership and reviewer admission checks. No dynamic schema comes from a model.
PROPOSAL_SCHEMA: dict[str, object] = {
    'type': 'object',
    'properties': {
        'task_id': {'type': 'string'},
        'files': {
            'type': 'array',
            'minItems': 1,
            'maxItems': 16,
            'items': {
                'type': 'object',
                'properties': {'path': {'type': 'string'}, 'content': {'type': 'string'}},
                'required': ['path', 'content'],
                'additionalProperties': False,
            },
        },
    },
    'required': ['task_id', 'files'],
    'additionalProperties': False,
}

REVIEW_SCHEMA: dict[str, object] = {
    'type': 'object',
    'properties': {
        'verdict': {'type': 'string', 'enum': ['accept', 'changes_required']},
        'findings': {
            'type': 'array',
            'maxItems': 20,
            'items': {'type': 'string'},
        },
    },
    'required': ['verdict', 'findings'],
    'additionalProperties': False,
}


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(  # noqa: PLR0913, PLR0917
        self,
        request: urllib.request.Request,
        fp: object,
        code: int,
        msg: str,
        headers: object,
        newurl: str,
    ) -> None:
        _ = request, fp, code, msg, headers, newurl


def _native_url(endpoint: LocalEndpoint) -> str:
    endpoint.validate()
    parsed = urlsplit(endpoint.url)
    if parsed.hostname not in ('127.0.0.1', '::1'):
        msg = 'Ollama-native transport requires loopback'
        raise LocalInferenceError(msg)
    address = '[::1]' if parsed.hostname == '::1' else '127.0.0.1'
    return f'http://{address}:{parsed.port or 80}/api/chat'


@dataclass(frozen=True, slots=True)
class NativeRequestOptions:
    schema: dict[str, object]
    think: bool
    temperature: float


def _native_payload(
    endpoint: LocalEndpoint, payload: bytes, *, options: NativeRequestOptions
) -> bytes:
    try:
        request_data: object = json.loads(payload)
    except (ValueError, UnicodeError) as exc:
        msg = 'Invalid chat request payload'
        raise LocalInferenceError(msg) from exc
    if not isinstance(request_data, dict):
        msg = 'Invalid chat request contract'
        raise LocalInferenceError(msg)
    valid = (
        request_data.get('model') == endpoint.model
        and request_data.get('stream') is False
        and isinstance(request_data.get('messages'), list)
    )
    if not valid:
        msg = 'Invalid chat request contract'
        raise LocalInferenceError(msg)
    return json.dumps({
        'model': endpoint.model,
        'stream': False,
        'think': options.think,
        'format': options.schema,
        'options': {'temperature': options.temperature, 'num_predict': endpoint.max_tokens},
        'messages': request_data['messages'],
    }).encode('utf-8')


def _read_native_response(
    opener: urllib.request.OpenerDirector, request: urllib.request.Request, timeout: float
) -> bytes:
    try:
        with opener.open(request, timeout=timeout) as response:
            if response.status != HTTP_OK:
                msg = 'Ollama returned unsuccessful HTTP status'
                raise LocalInferenceError(msg)
            raw = response.read(MAX_RESPONSE_BYTES + 1)
    except (OSError, urllib.error.URLError) as exc:
        msg = 'Ollama native inference request failed'
        raise LocalInferenceError(msg) from exc
    if len(raw) > MAX_RESPONSE_BYTES:
        msg = 'Ollama response exceeds limit'
        raise LocalInferenceError(msg)
    return cast('bytes', raw)


def _as_chat_completion(raw: bytes) -> bytes:
    try:
        document: object = json.loads(raw)
    except (ValueError, UnicodeError) as exc:
        msg = 'Malformed Ollama native response'
        raise LocalInferenceError(msg) from exc
    if not isinstance(document, dict) or not isinstance(document.get('message'), dict):
        msg = 'Invalid Ollama native response'
        raise LocalInferenceError(msg)
    content = document['message'].get('content')
    if not isinstance(content, str) or not content.strip():
        msg = 'Ollama returned empty final content'
        raise LocalInferenceError(msg)
    result = json.dumps({'choices': [{'message': {'content': content}}]}).encode('utf-8')
    if len(result) > MAX_RESPONSE_BYTES:
        msg = 'Converted Ollama response exceeds limit'
        raise LocalInferenceError(msg)
    return result


@dataclass(frozen=True, slots=True)
class NativeOllamaJSONTransport:
    """Adapt allowlisted chat requests to Ollama /api/chat, without tools or redirects."""

    think: bool = False
    temperature: float = 0.0
    # Explicit mapping prevents inferring task roles from untrusted model prompts.
    schemas_by_model: Mapping[str, dict[str, object]] = field(default_factory=dict)
    _opener: urllib.request.OpenerDirector | None = field(default=None, repr=False, compare=False)

    def complete(self, endpoint: LocalEndpoint, payload: bytes) -> bytes:
        """Return the original pipeline's one-choice response representation."""
        url = _native_url(endpoint)
        schema = self.schemas_by_model.get(endpoint.model)
        if schema is None:
            msg = 'No operator-approved JSON schema for local model'
            raise LocalInferenceError(msg)
        native_payload = _native_payload(
            endpoint,
            payload,
            options=NativeRequestOptions(schema, self.think, self.temperature),
        )
        request = urllib.request.Request(  # noqa: S310 - validated loopback HTTP URL
            url,
            data=native_payload,
            method='POST',
            headers={'Content-Type': 'application/json'},
        )
        opener = self._opener or urllib.request.build_opener(
            urllib.request.ProxyHandler({}), _NoRedirect()
        )
        return _as_chat_completion(_read_native_response(opener, request, endpoint.timeout_seconds))
