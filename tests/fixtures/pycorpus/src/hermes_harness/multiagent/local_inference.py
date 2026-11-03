"""Bounded opt-in local chat-completions inference; no tool execution.

This is NOT wired to the offline worker factory. Responses are untrusted text
and must pass the canonical structured-plan admission boundary.
"""

import json
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol
from urllib.parse import SplitResult, urlsplit

from hermes_harness.multiagent.model_edits import parse_edit_proposal
from hermes_harness.multiagent.plan_io import parse_plan_json
from hermes_harness.multiagent.replacement_proposals import parse_replacement_proposal
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from hermes_harness.multiagent.model_edits import EditProposal
    from hermes_harness.multiagent.replace_edits import FileReplacement
    from hermes_harness.multiagent.runtime import Plan

MAX_RESPONSE_BYTES = 256 * 1024
MAX_PROMPT_CHARS = 16_384

MAX_MODEL_ID_LENGTH = 128

MAX_INFERENCE_SECONDS = 120

MAX_INFERENCE_TOKENS = 8192

HTTP_OK = 200

MAX_REPLACEMENT_PREIMAGES = 16

SHA256_HEX_LENGTH = 64


def _allowed_chat_url(parsed: object) -> bool:
    """Reject URL credentials, query/fragment and remote or alternate endpoints."""
    if not isinstance(parsed, SplitResult):
        return False
    endpoint_ok = parsed.scheme == 'http' and parsed.hostname in ('127.0.0.1', '::1')
    auth_free = parsed.username is None and parsed.password is None
    canonical = parsed.path == '/v1/chat/completions' and not (parsed.query or parsed.fragment)
    return endpoint_ok and auth_free and canonical


class LocalInferenceError(RuntimeError):
    """Configured local inference failed or returned an invalid response."""


@dataclass(frozen=True, slots=True)
class LocalEndpoint:
    """Allowlisted local HTTP endpoint, model and bounded inference budget."""

    url: str
    model: str
    timeout_seconds: float = 30.0
    max_tokens: int = 2048

    def validate(self) -> None:
        parsed = urlsplit(self.url)
        if not _allowed_chat_url(parsed):
            msg = 'Local inference requires an explicit loopback chat-completions URL'
            raise LocalInferenceError(msg)
        if not self.model.strip() or len(self.model) > MAX_MODEL_ID_LENGTH:
            msg = 'Invalid local model identifier'
            raise LocalInferenceError(msg)
        if (
            not 1 <= self.timeout_seconds <= MAX_INFERENCE_SECONDS
            or not 1 <= self.max_tokens <= MAX_INFERENCE_TOKENS
        ):
            msg = 'Invalid inference budget'
            raise LocalInferenceError(msg)


class Transport(Protocol):
    """Injectable transport for model-response qualification and offline tests."""

    def complete(self, endpoint: LocalEndpoint, payload: bytes) -> bytes: ...


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
        _ = (request, fp, code, msg, headers, newurl)


class LoopbackTransport:
    """Explicit loopback HTTP request; no proxy, redirect or credential handling."""

    def complete(self, endpoint: LocalEndpoint, payload: bytes) -> bytes:
        endpoint.validate()
        request = urllib.request.Request(  # noqa: S310
            endpoint.url,
            data=payload,
            method='POST',
            headers={'Content-Type': 'application/json'},
        )
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect())
        try:
            with opener.open(request, timeout=endpoint.timeout_seconds) as response:
                if response.status != HTTP_OK:
                    msg = 'Local endpoint returned unsuccessful HTTP status'
                    raise LocalInferenceError(msg)
                data: bytes = response.read(MAX_RESPONSE_BYTES + 1)
        except (OSError, urllib.error.URLError) as exc:
            msg = 'Local inference request failed'
            raise LocalInferenceError(msg) from exc
        if len(data) > MAX_RESPONSE_BYTES:
            msg = 'Local endpoint response exceeds limit'
            raise LocalInferenceError(msg)
        return data


def complete_text(  # noqa: C901, PLR0913, PLR0915, PLR0917 - keep validated public integration signature
    endpoint: LocalEndpoint, system: str, prompt: str, transport: Transport
) -> str:
    """Return bounded model text; output is NOT trusted tool instructions."""
    endpoint.validate()
    if not system or not prompt or len(system) + len(prompt) > MAX_PROMPT_CHARS:
        msg = 'Invalid or oversized inference input'
        raise LocalInferenceError(msg)
    payload = json.dumps({
        'model': endpoint.model,
        'stream': False,
        'max_tokens': endpoint.max_tokens,
        'messages': [
            {'role': 'system', 'content': system},
            {'role': 'user', 'content': prompt},
        ],
    }).encode('utf-8')
    raw = transport.complete(endpoint, payload)
    if len(raw) > MAX_RESPONSE_BYTES:
        msg = 'Unbounded local model response'
        raise LocalInferenceError(msg)
    try:
        document: object = json.loads(raw)
    except (ValueError, UnicodeError) as exc:
        msg = 'Invalid local inference JSON'
        raise LocalInferenceError(msg) from exc
    if not isinstance(document, dict):
        msg = 'Invalid local inference response'
        raise LocalInferenceError(msg)
    choices = document.get('choices')
    if not isinstance(choices, list) or len(choices) != 1:
        msg = 'Local inference must return one choice'
        raise LocalInferenceError(msg)
    choice = choices[0]
    if not isinstance(choice, dict):
        msg = 'Invalid local inference choice'
        raise LocalInferenceError(msg)
    message = choice.get('message')
    if not isinstance(message, dict) or not isinstance(message.get('content'), str):
        msg = 'Missing local inference content'
        raise LocalInferenceError(msg)
    text = message['content']
    if not text.strip() or len(text) > MAX_RESPONSE_BYTES:
        msg = 'Empty or oversized local model text'
        raise LocalInferenceError(msg)
    return str(text)


def propose_plan(endpoint: LocalEndpoint, task: str, transport: Transport) -> Plan:
    """Ask a configured local model for a plan; validate before any dispatch."""
    system = (
        'Return ONLY one JSON plan with id, objective, primary_identity, nodes. '
        'Each node has id, objective, owned_paths, dependencies and role. '
        'Use coding by default. No markdown or prose.'
    )
    output = complete_text(endpoint, system, task, transport)
    try:
        return parse_plan_json(output)
    except WorkflowError as exc:
        msg = 'Local coordinator output failed plan admission'
        raise LocalInferenceError(msg) from exc


def propose_edits(  # noqa: PLR0913, PLR0917 - keep validated public integration signature
    endpoint: LocalEndpoint,
    task_id: str,
    objective: str,
    owned_paths: tuple[str, ...],
    transport: Transport,
) -> EditProposal:
    """Generate bounded proposed file content without applying or executing it."""
    system = (
        'Return ONLY JSON with task_id and files. Each file has path and content. '
        'Do not use markdown, shell commands, or paths outside the provided ownership.'
    )
    prompt = json.dumps({'task_id': task_id, 'objective': objective, 'owned_paths': owned_paths})
    text = complete_text(endpoint, system, prompt, transport)
    try:
        return parse_edit_proposal(text, task_id, owned_paths)
    except WorkflowError as exc:
        msg = 'Local coding output failed ownership admission'
        raise LocalInferenceError(msg) from exc


def propose_replacements(  # noqa: PLR0913, PLR0917 - keep validated public integration signature
    endpoint: LocalEndpoint,
    task_id: str,
    objective: str,
    owned_paths: tuple[str, ...],
    preimages: dict[str, str],
    transport: Transport,
) -> tuple[FileReplacement, ...]:
    """Generate existing-file replacements; model must echo preimage hashes."""
    if not preimages or len(preimages) > MAX_REPLACEMENT_PREIMAGES:
        msg = 'Expected bounded existing-file preimages'
        raise LocalInferenceError(msg)
    for name, digest in preimages.items():
        if not any(name == own or name.startswith(f'{own}/') for own in owned_paths):
            msg = 'Preimage path outside task ownership'
            raise LocalInferenceError(msg)
        if len(digest) != SHA256_HEX_LENGTH or any(ch not in '0123456789abcdef' for ch in digest):
            msg = 'Invalid preimage hash'
            raise LocalInferenceError(msg)
    system = (
        'Return ONLY JSON with task_id and replacements; each replacement has '
        'path, expected_sha256 and content. Use only paths and hashes supplied.'
    )
    prompt = json.dumps({
        'task_id': task_id,
        'objective': objective,
        'owned_paths': owned_paths,
        'preimages': preimages,
    })
    output = complete_text(endpoint, system, prompt, transport)
    try:
        replacements = parse_replacement_proposal(output, task_id, owned_paths)
    except WorkflowError as exc:
        msg = 'Local replacement response failed admission'
        raise LocalInferenceError(msg) from exc
    if any(preimages.get(item.path) != item.expected_sha256 for item in replacements):
        msg = 'Local replacement does not match supplied preimage'
        raise LocalInferenceError(msg)
    return replacements
