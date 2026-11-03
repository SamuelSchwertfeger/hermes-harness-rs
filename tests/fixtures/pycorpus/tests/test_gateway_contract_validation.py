import asyncio
from contextlib import asynccontextmanager
from typing import TYPE_CHECKING

import pytest

from hermes_harness.gateway.gateway_attachment import (
    GatewayAttachmentConfig,
    GatewayAttachmentError,
)
from hermes_harness.gateway.gateway_contract_validation import validate_existing_gateway
from hermes_harness.gateway.gateway_readonly import GatewayReadError

"""Offline safety checks for existing-gateway contract validation."""


MAX_READONLY_RPCS = 5

if TYPE_CHECKING:
    from collections.abc import AsyncIterator


class StubDiscovery:
    def __init__(self, *, available: bool) -> None:
        self.endpoint = object() if available else None
        self.state = 'ready' if available else 'absent'
        self.reason_code = None if available else 'absent'


class StubRPC:
    def __init__(self, *, broken: str = '') -> None:
        self.calls: list[tuple[str, dict[str, object]]] = []
        self.broken = broken

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        if method == self.broken:
            return []
        if method == 'ping':
            return {'pong': True}
        if method == 'session.list':
            return {'scope': 'live', 'sessions': []}
        if method == 'session.info':
            return {'stored_session_id': params['session_id']}
        if method == 'session.events.since':
            return self._replay_response(params)
        msg = 'forbidden RPC method'
        raise AssertionError(msg)

    def _replay_response(self, params: dict[str, object]) -> object:
        epoch = 'epoch-1'
        truncated = 'replay_epoch' not in params
        snapshot_required = truncated
        if self.broken == 'epoch_change' and 'replay_epoch' in params:
            epoch = 'epoch-2'
        elif self.broken == 'snapshot_required' and 'replay_epoch' in params:
            truncated = True
            snapshot_required = True
        return {
            'replay_epoch': epoch,
            'epoch': epoch,
            'latest_seq': 0,
            'events': [],
            'truncated': truncated,
            'snapshot_required': snapshot_required,
        }


class Dependencies:
    def __init__(self, *, available: bool = True, broken: str = '') -> None:
        self.discovery = StubDiscovery(available=available)
        self.rpc = StubRPC(broken=broken)
        self.endpoints: list[object] = []

    def discover(self, _profile_home: str) -> StubDiscovery:
        return self.discovery

    def attachable(self, result: StubDiscovery) -> bool:
        return result.endpoint is not None

    @asynccontextmanager
    async def connector(self, *, endpoint: object) -> AsyncIterator[StubRPC]:
        self.endpoints.append(endpoint)
        yield self.rpc

    def config(self) -> GatewayAttachmentConfig:
        return GatewayAttachmentConfig(
            '/home/hermes/.hermes', self.discover, self.attachable, self.connector
        )


def test_inactive_gateway_is_never_started() -> None:
    deps = Dependencies(available=False)
    with pytest.raises(GatewayAttachmentError, match='Existing gateway readiness timed out'):
        asyncio.run(validate_existing_gateway(deps.config()))
    assert deps.rpc.calls == []
    assert deps.endpoints == []


def test_no_session_id_uses_only_ping_and_list() -> None:
    deps = Dependencies()
    report = asyncio.run(validate_existing_gateway(deps.config()))
    assert report.ping_verified
    assert report.sampled_live_session_count == 0
    assert not report.info_verified
    assert report.replay_latest_seq is None
    assert [method for method, _ in deps.rpc.calls] == ['ping', 'session.list']
    assert deps.endpoints == [deps.discovery.endpoint]


def test_explicit_session_checks_readonly_shapes() -> None:
    deps = Dependencies()
    report = asyncio.run(validate_existing_gateway(deps.config(), 'session-1'))
    assert report.info_verified
    assert report.replay_epoch_present
    assert report.replay_latest_seq == 0
    assert report.replay_consistent
    assert [method for method, _ in deps.rpc.calls] == [
        'ping',
        'session.list',
        'session.info',
        'session.events.since',
        'session.events.since',
    ]
    assert deps.rpc.calls[-2][1] == {'session_id': 'session-1', 'last_sequence': 0}
    assert deps.rpc.calls[-1][1] == {
        'session_id': 'session-1',
        'last_sequence': 0,
        'replay_epoch': 'epoch-1',
    }


@pytest.mark.parametrize('method', ['ping', 'session.list', 'session.info', 'session.events.since'])
def test_invalid_response_is_rejected(method: str) -> None:
    deps = Dependencies(broken=method)
    with pytest.raises(GatewayReadError):
        asyncio.run(validate_existing_gateway(deps.config(), 'session-1'))
    assert len(deps.rpc.calls) <= MAX_READONLY_RPCS


def test_blank_session_refused_before_discovery() -> None:
    deps = Dependencies()
    with pytest.raises(ValueError, match='session_id'):
        asyncio.run(validate_existing_gateway(deps.config(), ' '))
    assert deps.endpoints == []


@pytest.mark.parametrize('broken', ['epoch_change', 'snapshot_required'])
def test_unstable_replay_is_rejected(broken: str) -> None:
    deps = Dependencies(broken=broken)
    with pytest.raises(GatewayReadError):
        asyncio.run(validate_existing_gateway(deps.config(), 'session-1'))
    assert len(deps.rpc.calls) == MAX_READONLY_RPCS
