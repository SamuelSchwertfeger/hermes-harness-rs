"""Attach-only gateway discovery tests; no Hermes runtime is started."""

import asyncio
from contextlib import asynccontextmanager
from types import SimpleNamespace
from typing import TYPE_CHECKING

import pytest

from hermes_harness.gateway.gateway_attachment import (
    GatewayAttachmentConfig,
    GatewayAttachmentError,
    attach_existing_gateway,
)

if TYPE_CHECKING:
    from collections.abc import AsyncIterator


class StubRPC:
    async def rpc(self, _method: str, **_params: object) -> object:
        return {'ok': True}


def test_attaches_only_to_discovered_endpoint() -> None:
    endpoint = object()
    calls: list[object] = []

    def discover(home: str) -> object:
        calls.append(('discover', home))
        return SimpleNamespace(endpoint=endpoint, state='ready')

    @asynccontextmanager
    async def connector(*, endpoint: object) -> AsyncIterator[StubRPC]:
        calls.append(('connect', endpoint))
        yield StubRPC()

    config = GatewayAttachmentConfig(
        '/profile', discover, lambda result: result.state == 'ready', connector
    )

    async def run() -> None:
        async with attach_existing_gateway(config) as rpc:
            assert await rpc.rpc('ping') == {'ok': True}

    asyncio.run(run())
    assert calls == [('discover', '/profile'), ('connect', endpoint)]


@pytest.mark.parametrize(
    'result',
    [
        SimpleNamespace(endpoint=None, state='missing'),
        SimpleNamespace(endpoint=object(), state='inactive'),
    ],
)
def test_refuses_non_attachable_gateway_without_connecting(result: object) -> None:
    calls: list[str] = []

    def discover(_home: str) -> object:
        calls.append('discover')
        return result

    @asynccontextmanager
    async def connector(*, endpoint: object) -> AsyncIterator[StubRPC]:
        del endpoint
        calls.append('connect')
        yield StubRPC()

    config = GatewayAttachmentConfig(
        '/profile', discover, lambda _item: _item.state == 'ready', connector
    )
    with pytest.raises(GatewayAttachmentError, match='no start attempted'):
        asyncio.run(_run_context(config))
    assert calls == ['discover']


async def _run_context(config: GatewayAttachmentConfig) -> None:
    async with attach_existing_gateway(config):
        pytest.fail('non-attachable gateway must not connect')


def test_discovery_exception_does_not_attempt_connector() -> None:
    calls: list[str] = []

    def discover(_home: str) -> object:
        calls.append('discover')
        msg = 'private detail'
        raise PermissionError(msg)

    @asynccontextmanager
    async def connector(*, endpoint: object) -> AsyncIterator[StubRPC]:
        del endpoint
        calls.append('connect')
        yield StubRPC()

    config = GatewayAttachmentConfig('/profile', discover, lambda _item: True, connector)
    with pytest.raises(GatewayAttachmentError, match='discovery failed'):
        asyncio.run(_run_context(config))
    assert calls == ['discover']


def test_waits_for_absent_then_starting_then_ready() -> None:
    endpoint = object()
    states = iter(('absent', 'starting', 'ready'))
    calls: list[str] = []

    def discover(_home: str) -> object:
        state = next(states)
        calls.append(state)
        return SimpleNamespace(state=state, endpoint=endpoint if state == 'ready' else None)

    @asynccontextmanager
    async def connector(*, endpoint: object) -> AsyncIterator[StubRPC]:
        assert endpoint is expected_endpoint
        calls.append('connect')
        yield StubRPC()

    expected_endpoint = endpoint
    config = GatewayAttachmentConfig(
        '/profile',
        discover,
        lambda item: item.state == 'ready',
        connector,
        readiness_timeout=1.0,
        poll_interval=0.001,
    )
    asyncio.run(_run_success(config))
    assert calls == ['absent', 'starting', 'ready', 'connect']


async def _run_success(config: GatewayAttachmentConfig) -> None:
    async with attach_existing_gateway(config) as rpc:
        assert await rpc.rpc('ping') == {'ok': True}


def test_readiness_timeout_never_connects() -> None:
    calls: list[str] = []

    def discover(_home: str) -> object:
        calls.append('discover')
        return SimpleNamespace(state='starting', endpoint=None)

    @asynccontextmanager
    async def connector(*, endpoint: object) -> AsyncIterator[StubRPC]:
        del endpoint
        calls.append('connect')
        yield StubRPC()

    config = GatewayAttachmentConfig(
        '/profile',
        discover,
        lambda _item: False,
        connector,
        readiness_timeout=0.01,
        poll_interval=0.001,
    )
    with pytest.raises(GatewayAttachmentError, match='timed out'):
        asyncio.run(_run_context(config))
    assert 'discover' in calls
    assert 'connect' not in calls


def test_permanent_inaccessible_fails_without_polling() -> None:
    calls: list[str] = []

    def discover(_home: str) -> object:
        calls.append('discover')
        return SimpleNamespace(state='inaccessible', endpoint=None)

    @asynccontextmanager
    async def connector(*, endpoint: object) -> AsyncIterator[StubRPC]:
        del endpoint
        calls.append('connect')
        yield StubRPC()

    config = GatewayAttachmentConfig(
        '/profile',
        discover,
        lambda _item: False,
        connector,
    )
    with pytest.raises(GatewayAttachmentError, match='no start attempted'):
        asyncio.run(_run_context(config))
    assert calls == ['discover']
