"""Offline contract tests for bounded read-only Hermes session discovery."""

import asyncio
from contextlib import asynccontextmanager
from typing import TYPE_CHECKING

import pytest

from hermes_harness.gateway.gateway_attachment import (
    GatewayAttachmentConfig,
    GatewayAttachmentError,
)
from hermes_harness.gateway.gateway_readonly import GatewayReadError
from hermes_harness.gateway.gateway_session_discovery import discover_existing_sessions

if TYPE_CHECKING:
    from collections.abc import AsyncIterator


class Discovery:
    def __init__(self, *, available: bool = True) -> None:
        self.endpoint = object() if available else None
        self.state = 'ready' if available else 'absent'
        self.reason_code = None


class RPC:
    def __init__(self, response: object) -> None:
        self.response = response
        self.calls: list[tuple[str, dict[str, object]]] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        if isinstance(self.response, ConnectionError):
            raise self.response
        return self.response


class Dependencies:
    def __init__(self, response: object, *, available: bool = True) -> None:
        self.discovery = Discovery(available=available)
        self.client = RPC(response)
        self.connected = False

    def discover(self, _home: str) -> Discovery:
        return self.discovery

    def attachable(self, discovery: Discovery) -> bool:
        return discovery.endpoint is not None

    @asynccontextmanager
    async def connect(self, *, endpoint: object) -> AsyncIterator[RPC]:
        self.connected = endpoint is self.discovery.endpoint
        yield self.client

    def config(self) -> GatewayAttachmentConfig:
        return GatewayAttachmentConfig(
            '/home/hermes/.hermes', self.discover, self.attachable, self.connect
        )


def listing(scope: str = 'stored') -> dict[str, object]:
    return {
        'scope': scope,
        'sessions': [{'session_id': 'local-001', 'running': False, 'title': 'private'}],
    }


def test_stored_session_projection_excludes_private_metadata() -> None:
    deps = Dependencies(listing())
    result = asyncio.run(discover_existing_sessions(deps.config()))
    assert deps.connected
    assert result.scope == 'stored'
    assert len(result.sessions) == 1
    assert result.sessions[0].session_id == 'local-001'
    assert not hasattr(result.sessions[0], 'title')
    assert deps.client.calls == [('session.list', {'scope': 'stored', 'limit': 10})]


def test_empty_live_listing_is_valid() -> None:
    deps = Dependencies({'scope': 'live', 'sessions': []})
    result = asyncio.run(discover_existing_sessions(deps.config(), scope='live', limit=1))
    assert result.sessions == ()
    assert deps.client.calls == [('session.list', {'scope': 'live', 'limit': 1})]


@pytest.mark.parametrize(
    'bad',
    [
        {'scope': 'live', 'sessions': []},
        {'scope': 'stored', 'sessions': [{'session_id': 'a', 'running': 'no'}]},
        {'scope': 'stored', 'sessions': [{'session_id': '', 'running': False}]},
        {'scope': 'stored', 'sessions': [{'session_id': 'a', 'running': False}] * 2},
        {'scope': 'stored', 'sessions': 'not-a-list'},
    ],
)
def test_invalid_or_unauthorized_listing_fails_closed(bad: object) -> None:
    deps = Dependencies(bad)
    with pytest.raises(GatewayReadError):
        asyncio.run(discover_existing_sessions(deps.config()))
    assert len(deps.client.calls) == 1


def test_oversized_listing_is_rejected() -> None:
    rows = [
        {'session_id': 'a', 'running': False},
        {'session_id': 'b', 'running': False},
    ]
    deps = Dependencies({'scope': 'stored', 'sessions': rows})
    with pytest.raises(GatewayReadError):
        asyncio.run(discover_existing_sessions(deps.config(), limit=1))
    assert len(deps.client.calls) == 1


@pytest.mark.parametrize('limit', [0, 21, True, 1.5])
def test_invalid_limit_refused_before_discovery(limit: object) -> None:
    deps = Dependencies(listing())
    with pytest.raises(ValueError, match='limit must be an integer'):
        asyncio.run(discover_existing_sessions(deps.config(), limit=limit))
    assert not deps.client.calls


def test_absent_gateway_does_not_connect() -> None:
    deps = Dependencies(listing(), available=False)
    with pytest.raises(GatewayAttachmentError):
        asyncio.run(discover_existing_sessions(deps.config()))
    assert not deps.connected


def test_disconnect_is_propagated_without_retry() -> None:
    deps = Dependencies(ConnectionError('disconnected'))
    with pytest.raises(ConnectionError):
        asyncio.run(discover_existing_sessions(deps.config()))
    assert len(deps.client.calls) == 1
