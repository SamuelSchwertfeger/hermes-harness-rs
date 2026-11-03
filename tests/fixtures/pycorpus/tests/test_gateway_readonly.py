"""Offline tests for the injected, read-only Hermes connection boundary."""

import asyncio
from collections.abc import Awaitable, Callable  # noqa: TC003 - runtime test annotations

import pytest

from hermes_harness.gateway.gateway_readonly import GatewayReadError, read_from_hermes, read_session


class FakeRPC:
    def __init__(self, response: object = None, failure: Exception | None = None) -> None:
        self.calls: list[tuple[str, dict[str, str]]] = []
        self.response = response
        self.failure = failure

    async def rpc(self, method: str, **params: str) -> object:
        self.calls.append((method, params))
        if self.failure is not None:
            raise self.failure
        return self.response


async def check_read_only_method_and_identity() -> None:
    rpc = FakeRPC({'title': 'isolated'})
    result = await read_session(rpc, 'session-123')
    assert result.session_id == 'session-123'
    assert result.data == {'title': 'isolated'}
    assert rpc.calls == [('session.info', {'session_id': 'session-123'})]


async def check_invalid_session_refused_without_rpc() -> None:
    rpc = FakeRPC({})
    with pytest.raises(ValueError, match='session_id'):
        await read_session(rpc, ' ')
    assert not rpc.calls


async def check_invalid_payload_rejected() -> None:
    with pytest.raises(GatewayReadError, match='string keys'):
        await read_session(FakeRPC({1: 'invalid'}), 'session-123')


async def check_disconnect_never_retries() -> None:
    rpc = FakeRPC(failure=ConnectionError())
    with pytest.raises(GatewayReadError, match='no retry'):
        await read_session(rpc, 'session-123')
    assert len(rpc.calls) == 1


class FakeConnection:
    def __init__(self, rpc: FakeRPC) -> None:
        self._rpc = rpc

    async def __aenter__(self) -> FakeRPC:
        return self._rpc

    async def __aexit__(self, *_exc: object) -> None:
        return None


async def check_no_endpoint_does_not_invoke_connector() -> None:
    endpoints: list[object] = []

    def connector(*, endpoint: object) -> FakeConnection:
        endpoints.append(endpoint)
        return FakeConnection(FakeRPC())

    with pytest.raises(ValueError, match='mandatory'):
        await read_from_hermes(None, 'session-123', connector=connector)
    assert not endpoints


async def check_injected_connector_uses_explicit_endpoint_only() -> None:
    endpoint = object()
    rpc = FakeRPC({'status': 'opaque'})
    endpoints: list[object] = []

    def connector(*, endpoint: object) -> FakeConnection:
        endpoints.append(endpoint)
        return FakeConnection(rpc)

    result = await read_from_hermes(endpoint, 'session-123', connector=connector)
    assert endpoints == [endpoint]
    assert result.data['status'] == 'opaque'
    assert [call[0] for call in rpc.calls] == ['session.info']


@pytest.mark.parametrize(
    'case',
    [
        check_read_only_method_and_identity,
        check_invalid_session_refused_without_rpc,
        check_invalid_payload_rejected,
        check_disconnect_never_retries,
        check_no_endpoint_does_not_invoke_connector,
        check_injected_connector_uses_explicit_endpoint_only,
    ],
)
def test_async_case(case: Callable[[], Awaitable[None]]) -> None:
    asyncio.run(case())
