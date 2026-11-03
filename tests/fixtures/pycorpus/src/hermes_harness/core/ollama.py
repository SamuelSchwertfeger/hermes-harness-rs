"""Single-lane tool-free Ollama requests. No Hermes gateway work is created."""

import fcntl
import json
import urllib.error
import urllib.request
from collections.abc import Iterator  # noqa: TC003 - runtime annotation on Python 3.13
from contextlib import contextmanager
from dataclasses import dataclass
from http import HTTPStatus
from pathlib import Path
from typing import Protocol

from hermes_harness.core.routing import Role, Route, select  # noqa: TC001

ENDPOINT = 'http://127.0.0.1:11434/api/generate'
MAX_RESPONSE_BYTES = 1_000_000


@dataclass(frozen=True, kw_only=True)
class OllamaOptions:
    url: str = ENDPOINT
    lock_file: Path = Path('.harness-runtime/gpu.lock')


@contextmanager
def gpu_lane(lock_file: Path) -> Iterator[None]:
    lock_file.parent.mkdir(parents=True, exist_ok=True)
    with lock_file.open('a+') as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _decode(body: bytes) -> str:
    data: object = json.loads(body)
    if not isinstance(data, dict):
        msg = 'Invalid Ollama response: expected JSON object'
        raise TypeError(msg)
    result = data.get('response')
    if not isinstance(result, str):
        msg = 'Invalid Ollama response: expected string response'
        raise TypeError(msg)
    return result


def _request(prompt: str, route: Route, url: str) -> urllib.request.Request:
    # Exact endpoint check prevents non-HTTP schemes, SSRF and accidental remote calls.
    if url != ENDPOINT:
        msg = 'Only the loopback Ollama endpoint is enabled in milestone 1'
        raise PermissionError(msg)
    payload = json.dumps({
        'model': route.model,
        'prompt': prompt,
        'stream': False,
        'options': {'num_predict': route.max_output_tokens, 'num_ctx': 8192},
        'keep_alive': '0s',
    }).encode()
    return urllib.request.Request(  # noqa: S310 - exact allowlisted HTTP URL
        url,
        data=payload,
        headers={'Content-Type': 'application/json'},
        method='POST',
    )


class _HTTPResponse(Protocol):
    status: int

    def read(self, size: int) -> bytes: ...


def _read_response(response: _HTTPResponse) -> bytes:
    status = response.status
    if status != HTTPStatus.OK:
        msg = f'Ollama HTTP status {status}'
        raise RuntimeError(msg)
    body = response.read(MAX_RESPONSE_BYTES + 1)
    if not isinstance(body, bytes):
        msg = 'Ollama returned a non-bytes HTTP body'
        raise TypeError(msg)
    return body


def _fetch(request: urllib.request.Request, timeout: int) -> bytes:
    try:
        with urllib.request.urlopen(  # noqa: S310 - request URL exact-allowlisted
            request, timeout=timeout
        ) as response:
            body = _read_response(response)
    except (urllib.error.URLError, TimeoutError) as exc:
        msg = 'Ollama request failed; verify idle state before retry'
        raise RuntimeError(msg) from exc
    if len(body) > MAX_RESPONSE_BYTES:
        msg = 'Ollama response exceeds byte budget'
        raise ValueError(msg)
    return body


def text_task(
    prompt: str,
    *,
    role: Role = Role.SCOUT,
    options: OllamaOptions | None = None,
) -> str:
    route = select(role)
    if route.backend != 'ollama' or route.tool_access:
        msg = 'Only tool-free local routes supported by this adapter'
        raise PermissionError(msg)
    opts = options or OllamaOptions()
    request = _request(prompt, route, opts.url)
    with gpu_lane(opts.lock_file):
        body = _fetch(request, route.max_seconds)
    return _decode(body)
