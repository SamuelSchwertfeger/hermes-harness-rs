"""Explicit Ollama/llama.cpp model registry and credential-safe inference transport.

Only operator-authored profiles may route model calls. TLS verification is mandatory
for remote inference, and no configured endpoint implies host-change authority.
"""

import json
import os
import re
import ssl
import urllib.error
import urllib.request
from dataclasses import dataclass
from http import HTTPStatus
from typing import TYPE_CHECKING, Protocol
from urllib.parse import urlsplit

from hermes_harness.multiagent.local_inference import (
    MAX_RESPONSE_BYTES,
    LocalEndpoint,
    LocalInferenceError,
)
from hermes_harness.multiagent.ollama_native import NativeOllamaJSONTransport

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.multiagent.local_inference import (
        Transport,
    )

MAX_REGISTRY_BYTES = 32_768
MIN_HERMES_CONTEXT = 65_536
MAX_CONTEXT = 262_144
MAX_PROVIDER_TIMEOUT_SECONDS = 120
MAX_PROVIDER_OUTPUT_TOKENS = 8192


class ProviderError(LocalInferenceError):
    """Categorized local provider integration failure (never includes secrets)."""


@dataclass(frozen=True, slots=True)
class ProviderSpec:
    """Operator-controlled inference placement, not a model self-declaration."""

    name: str
    backend: str
    model: str
    url: str
    context_tokens: int
    key_env: str = ''
    tool_calls: bool = False
    unit: str = ''
    gpu_host: str = ''
    max_concurrency: int = 1

    def validate(self) -> None:  # noqa: C901, PLR0912, PLR0915
        invalid_model = re.fullmatch(
            r'[a-zA-Z0-9_][a-zA-Z0-9_.:/-]{0,127}', self.model
        ) is None or any(part in ('', '.', '..') for part in self.model.split('/'))
        if not self.name or invalid_model or self.backend not in {'ollama', 'llama.cpp'}:
            msg = 'invalid-provider-identity'
            raise ProviderError(msg)
        if not 1 <= self.context_tokens <= MAX_CONTEXT or self.max_concurrency != 1:
            msg = 'invalid-context-or-concurrency'
            raise ProviderError(msg)
        parts = urlsplit(self.url)
        if parts.username or parts.password or parts.query or parts.fragment:  # noqa: PLR0916
            msg = 'unsafe-endpoint-url'
            raise ProviderError(msg)
        if parts.path != '/v1/chat/completions':
            msg = 'invalid-endpoint-path'
            raise ProviderError(msg)
        if self.backend == 'ollama':
            if parts.scheme != 'http' or parts.hostname not in {'127.0.0.1', '::1'}:
                msg = 'ollama-must-be-loopback'
                raise ProviderError(msg)
        elif (
            parts.scheme != 'https'
            or not parts.hostname
            or not parts.hostname.endswith('.home.arpa')
        ):
            msg = 'remote-endpoint-must-use-trusted-internal-https'
            raise ProviderError(msg)
        if self.key_env and (not self.key_env.isidentifier() or self.key_env.startswith('_')):
            msg = 'invalid-credential-reference'
            raise ProviderError(msg)
        if self.backend == 'llama.cpp' and not self.key_env:
            msg = 'remote-bearer-auth-required'
            raise ProviderError(msg)
        if self.backend == 'ollama' and self.key_env:
            msg = 'ollama-native-adapter-does-not-support-auth'
            raise ProviderError(msg)
        if self.unit and (not self.unit.endswith('.service') or '/' in self.unit):
            msg = 'invalid-systemd-unit'
            raise ProviderError(msg)

    def admit_role(self, role: str) -> None:
        self.validate()
        if role in {'hermes-tool-worker', 'hermes-coordinator'}:
            if self.context_tokens < MIN_HERMES_CONTEXT:
                msg = 'insufficient-context-for-hermes-role'
                raise ProviderError(msg)
            if not self.tool_calls:
                msg = 'tool-calling-not-qualified'
                raise ProviderError(msg)
            msg = 'Hermes-native-tool-receipt-required-for-admission'
            raise ProviderError(msg)
        if role == 'security-local' and self.backend not in {'ollama', 'llama.cpp'}:
            msg = 'nonlocal-security-role'
            raise ProviderError(msg)


@dataclass(frozen=True, slots=True)
class ConfiguredEndpoint(LocalEndpoint):
    """Exactly one validated operator-approved endpoint (possibly remote HTTPS)."""

    spec: ProviderSpec | None = None

    def validate(self) -> None:
        if self.spec is None or self.spec.url != self.url or self.spec.model != self.model:
            msg = 'endpoint-does-not-match-registry'
            raise ProviderError(msg)
        self.spec.validate()
        if (
            not 1 <= self.timeout_seconds <= MAX_PROVIDER_TIMEOUT_SECONDS
            or not 1 <= self.max_tokens <= MAX_PROVIDER_OUTPUT_TOKENS
        ):
            msg = 'invalid-inference-budget'
            raise ProviderError(msg)
        if self.spec.key_env and not os.environ.get(self.spec.key_env):
            msg = 'credential-environment-missing'
            raise ProviderError(msg)


class Opener(Protocol):
    def open(self, request: urllib.request.Request, timeout: float) -> object: ...


OpenerLike = Opener | urllib.request.OpenerDirector


class NoRedirect(urllib.request.HTTPRedirectHandler):
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


def _opener() -> urllib.request.OpenerDirector:
    """Disable proxies and redirects, retain platform certificate verification."""
    return urllib.request.build_opener(
        urllib.request.ProxyHandler({}),
        NoRedirect(),
        urllib.request.HTTPSHandler(context=ssl.create_default_context()),
    )


def _http(opener: OpenerLike, request: urllib.request.Request, timeout: float) -> bytes:  # noqa: C901, PLR0912, PLR0915
    try:
        with opener.open(request, timeout=timeout) as reply:  # type: ignore[union-attr]
            if reply.status != HTTPStatus.OK:
                msg = 'provider-http-failure'
                raise ProviderError(msg)
            content: bytes = reply.read(MAX_RESPONSE_BYTES + 1)
    except urllib.error.HTTPError as exc:
        status = exc.code
        if status in (401, 403):
            category = 'provider-authentication-rejected'
        elif status == HTTPStatus.TOO_MANY_REQUESTS:
            category = 'provider-rate-limited'
        elif status >= HTTPStatus.INTERNAL_SERVER_ERROR:
            category = 'provider-server-failure'
        else:
            category = 'provider-http-failure'
        raise ProviderError(category) from None
    except TimeoutError:
        msg = 'provider-timeout'
        raise ProviderError(msg) from None
    except (urllib.error.URLError, OSError, ssl.SSLError) as exc:
        _ = exc
        msg = 'provider-unreachable-or-tls-failure'
        raise ProviderError(msg) from None
    if len(content) > MAX_RESPONSE_BYTES:
        msg = 'provider-response-too-large'
        raise ProviderError(msg)
    return content


@dataclass(frozen=True, slots=True)
class OpenAICompatibleTransport:
    """HTTPS llama.cpp adapter. Never prints the bearer token or raw HTTP errors."""

    opener: OpenerLike | None = None

    def complete(self, endpoint: LocalEndpoint, payload: bytes) -> bytes:
        if not isinstance(endpoint, ConfiguredEndpoint) or endpoint.spec is None:
            msg = 'unregistered-openai-endpoint'
            raise ProviderError(msg)
        endpoint.validate()
        if endpoint.spec.backend != 'llama.cpp':
            msg = 'incorrect-provider-backend'
            raise ProviderError(msg)
        key = os.environ[endpoint.spec.key_env]
        request = urllib.request.Request(  # noqa: S310
            endpoint.url,
            data=payload,
            method='POST',
            headers={
                'Content-Type': 'application/json',
                'Authorization': f'Bearer {key}',
            },
        )
        raw = _http(self.opener or _opener(), request, endpoint.timeout_seconds)
        try:
            value = json.loads(raw)
            if (
                not isinstance(value, dict)  # noqa: PLR0916
                or value.get('model') != endpoint.model
                or not isinstance(value.get('choices'), list)
                or len(value['choices']) != 1
            ):
                msg = 'completion-model-identity-or-schema-mismatch'
                raise ProviderError(msg)
        except (ValueError, TypeError) as exc:
            msg = 'malformed-provider-completion'
            raise ProviderError(msg) from exc
        return raw


@dataclass(frozen=True, slots=True)
class ProviderRegistry:
    """Role-to-profile map. No automatic backend or cloud fallback."""

    profiles: dict[str, ProviderSpec]
    roles: dict[str, str]

    def role(self, role: str) -> ProviderSpec:
        name = self.roles.get(role)
        if name is None or name not in self.profiles:
            msg = 'role-not-configured'
            raise ProviderError(msg)
        spec = self.profiles[name]
        spec.admit_role(role)
        return spec

    def endpoint(self, role: str) -> ConfiguredEndpoint:
        spec = self.role(role)
        return ConfiguredEndpoint(spec.url, spec.model, 120, 2048, spec)

    def transport(self, roles: tuple[str, ...], schemas: dict[str, dict[str, object]]) -> Transport:
        routes: dict[str, Transport] = {}
        ollama: dict[str, dict[str, object]] = {}
        for role in roles:
            spec = self.role(role)
            if spec.model in routes or spec.model in ollama:
                msg = 'duplicate-model-id-in-routes'
                raise ProviderError(msg)
            if spec.backend == 'ollama':
                ollama[spec.model] = schemas[role]
            else:
                routes[spec.model] = OpenAICompatibleTransport()
        if ollama:
            native = NativeOllamaJSONTransport(schemas_by_model=ollama)
            routes.update(dict.fromkeys(ollama, native))
        return RoutingTransport(routes)


@dataclass(frozen=True, slots=True)
class RoutingTransport:
    routes: dict[str, Transport]

    def complete(self, endpoint: LocalEndpoint, payload: bytes) -> bytes:
        transport = self.routes.get(endpoint.model)
        if transport is None:
            msg = 'model-not-in-approved-route'
            raise ProviderError(msg)
        return transport.complete(endpoint, payload)


def load_registry(path: Path) -> ProviderRegistry:  # noqa: C901, PLR0912, PLR0915
    """Read an operator-owned JSON file; reject symlinks and inline credentials."""
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_REGISTRY_BYTES:
        msg = 'invalid-provider-registry-file'
        raise ProviderError(msg)
    try:
        document: object = json.loads(path.read_text(encoding='utf-8'))
    except (ValueError, UnicodeError) as exc:
        msg = 'invalid-provider-registry-json'
        raise ProviderError(msg) from exc
    if not isinstance(document, dict) or set(document) != {'profiles', 'roles'}:
        msg = 'invalid-provider-registry-schema'
        raise ProviderError(msg)
    raw = document['profiles']
    roles = document['roles']
    if not isinstance(raw, list) or not isinstance(roles, dict):
        msg = 'invalid-provider-registry-schema'
        raise ProviderError(msg)
    fields = {
        'name',
        'backend',
        'model',
        'url',
        'context_tokens',
        'key_env',
        'tool_calls',
        'unit',
        'gpu_host',
        'max_concurrency',
    }
    profiles: dict[str, ProviderSpec] = {}
    for value in raw:
        if not isinstance(value, dict) or set(value) - fields:
            msg = 'unsupported-provider-registry-field'
            raise ProviderError(msg)
        try:
            spec = ProviderSpec(**value)
            spec.validate()
        except (TypeError, ValueError) as exc:
            msg = 'invalid-provider-registry-profile'
            raise ProviderError(msg) from exc
        if spec.name in profiles:
            msg = 'duplicate-provider-profile'
            raise ProviderError(msg)
        profiles[spec.name] = spec
    if any(not isinstance(k, str) or not isinstance(v, str) for k, v in roles.items()):
        msg = 'invalid-role-mapping'
        raise ProviderError(msg)
    return ProviderRegistry(profiles, roles)


def probe_provider(spec: ProviderSpec, *, opener: OpenerLike | None = None) -> dict[str, object]:  # noqa: C901, PLR0912, PLR0914, PLR0915
    """Bounded authenticated /models and chat probe; never asserts real 64K context."""
    spec.validate()
    endpoint = ConfiguredEndpoint(spec.url, spec.model, 15, 32, spec)
    endpoint.validate()
    opener = opener or _opener()
    parsed = urlsplit(spec.url)
    root = f'{parsed.scheme}://{parsed.netloc}'
    health_url = f'{root}/api/version' if spec.backend == 'ollama' else f'{root}/health'
    models_url = f'{root}/api/tags' if spec.backend == 'ollama' else f'{root}/v1/models'
    headers = {'Accept': 'application/json'}
    if spec.key_env:
        headers['Authorization'] = f'Bearer {os.environ[spec.key_env]}'
    # Some llama.cpp builds have /health without authentication; never trust it alone.
    health = _http(opener, urllib.request.Request(health_url, headers=headers), 15)  # noqa: S310
    models = _http(opener, urllib.request.Request(models_url, headers=headers), 15)  # noqa: S310
    try:
        catalogue = json.loads(models)
        if spec.backend == 'ollama':
            ids = [item['name'] for item in catalogue['models'] if isinstance(item, dict)]
        else:
            ids = [item['id'] for item in catalogue['data'] if isinstance(item, dict)]
    except (ValueError, KeyError, TypeError) as exc:
        msg = 'malformed-model-catalogue'
        raise ProviderError(msg) from exc
    if spec.model not in ids:
        msg = 'remote-model-identity-mismatch'
        raise ProviderError(msg)
    payload = json.dumps({
        'model': spec.model,
        'stream': False,
        'max_tokens': 8,
        'messages': [{'role': 'user', 'content': 'Reply OK.'}],
    }).encode()
    if spec.backend == 'llama.cpp':
        completion = OpenAICompatibleTransport(opener).complete(endpoint, payload)
    else:
        completion = NativeOllamaJSONTransport(
            schemas_by_model={
                spec.model: {'type': 'object', 'properties': {}, 'additionalProperties': True}
            },
        ).complete(endpoint, payload)
    try:
        document = json.loads(completion)
        choices = document['choices']
        if len(choices) != 1 or not isinstance(choices[0]['message']['content'], str):
            msg = 'malformed-provider-completion'
            raise ProviderError(msg)
        if spec.backend == 'llama.cpp' and document.get('model') != spec.model:
            msg = 'completion-model-identity-mismatch'
            raise ProviderError(msg)
    except (ValueError, KeyError, TypeError, IndexError) as exc:
        msg = 'malformed-provider-completion'
        raise ProviderError(msg) from exc
    return {
        'profile': spec.name,
        'backend': spec.backend,
        'model': spec.model,
        'health_response_received': bool(health),
        'model_identity_verified': True,
        'authenticated_completion': bool(spec.key_env),
        'declared_context_tokens': spec.context_tokens,
        'long_context_qualified': False,  # requires an independent long-prompt test
        'tool_calls_qualified': False,  # requires Hermes-native terminal receipts
        'usage_available': isinstance(document.get('usage'), dict),
    }
