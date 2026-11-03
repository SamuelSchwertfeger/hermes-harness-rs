"""Read-only Ollama discovery on the operator's loopback endpoint.

No model pull, load, unload, or remote inference is performed by discovery.
"""

import json
import urllib.error
import urllib.request
from dataclasses import dataclass
from urllib.parse import urlsplit

from hermes_harness.multiagent.local_inference import LocalInferenceError

MAX_TAGS_RESPONSE = 64 * 1024

MAX_OLLAMA_MODELS = 256

MAX_MODEL_NAME_LENGTH = 200

HTTP_OK = 200


@dataclass(frozen=True, slots=True)
class OllamaModel:
    name: str
    size_bytes: int


def _parse_model(item: object) -> OllamaModel:
    if not isinstance(item, dict):
        msg = 'Malformed Ollama model entry'
        raise LocalInferenceError(msg)
    name, size = item.get('name'), item.get('size')
    if not isinstance(name, str) or not name.strip() or len(name) > MAX_MODEL_NAME_LENGTH:
        msg = 'Invalid Ollama model name'
        raise LocalInferenceError(msg)
    if not isinstance(size, int) or isinstance(size, bool) or size < 0:
        msg = 'Invalid Ollama model size'
        raise LocalInferenceError(msg)
    return OllamaModel(name, size)


def parse_ollama_tags(data: bytes) -> tuple[OllamaModel, ...]:
    """Admit bounded Ollama /api/tags output without trusting arbitrary fields."""
    if len(data) > MAX_TAGS_RESPONSE:
        msg = 'Ollama model listing exceeds limit'
        raise LocalInferenceError(msg)
    try:
        document: object = json.loads(data)
    except (UnicodeError, ValueError) as exc:
        msg = 'Malformed Ollama model listing'
        raise LocalInferenceError(msg) from exc
    if not isinstance(document, dict) or not isinstance(document.get('models'), list):
        msg = 'Invalid Ollama model listing'
        raise LocalInferenceError(msg)
    rows = document['models']
    if len(rows) > MAX_OLLAMA_MODELS:
        msg = 'Too many Ollama models'
        raise LocalInferenceError(msg)
    return tuple(_parse_model(item) for item in rows)


def ollama_models(base_url: str = 'http://127.0.0.1:11434') -> tuple[OllamaModel, ...]:
    """Fetch model inventory only from explicit literal loopback endpoints."""
    parsed = urlsplit(base_url)
    endpoint_ok = parsed.scheme == 'http' and parsed.hostname in ('127.0.0.1', '::1')
    path_ok = parsed.path in ('', '/') and not (parsed.query or parsed.fragment)
    auth_free = parsed.username is None and parsed.password is None
    if not (endpoint_ok and path_ok and auth_free):
        msg = 'Ollama discovery endpoint must be loopback'
        raise LocalInferenceError(msg)
    url = base_url.rstrip('/') + '/api/tags'
    request = urllib.request.Request(url, method='GET')  # noqa: S310
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(request, timeout=5) as response:
            if response.status != HTTP_OK:
                msg = 'Ollama inventory request failed'
                raise LocalInferenceError(msg)
            raw: bytes = response.read(MAX_TAGS_RESPONSE + 1)
    except (OSError, urllib.error.URLError) as exc:
        msg = 'Ollama model inventory unavailable'
        raise LocalInferenceError(msg) from exc
    return parse_ollama_tags(raw)
