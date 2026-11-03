"""Attach to an existing Hermes gateway, waiting briefly for it to become ready."""

import asyncio
import time
from contextlib import asynccontextmanager
from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from contextlib import AbstractAsyncContextManager

DEFAULT_READINESS_TIMEOUT = 15.0
DEFAULT_READINESS_POLL_INTERVAL = 0.5
_TRANSIENT_STATES = frozenset({'absent', 'starting'})


class GatewayAttachmentError(RuntimeError):
    """The existing gateway could not be safely identified or attached."""


class DiscoveryResult(Protocol):
    """Minimum result required to make an attach-only decision."""

    endpoint: object | None


class DiscoverGateway(Protocol):
    def __call__(self, profile_home: str) -> DiscoveryResult: ...


class IsAttachable(Protocol):
    def __call__(self, result: DiscoveryResult) -> bool: ...


class GatewayRPC(Protocol):
    async def rpc(self, method: str, **params: object) -> object: ...


class Connector(Protocol):
    def __call__(self, *, endpoint: object) -> AbstractAsyncContextManager[GatewayRPC]: ...


@dataclass(frozen=True, slots=True)
class GatewayAttachmentConfig:
    """Explicit profile discovery and injected no-autostart connector."""

    profile_home: str
    discover: DiscoverGateway
    is_attachable: IsAttachable
    connector: Connector
    readiness_timeout: float = DEFAULT_READINESS_TIMEOUT
    poll_interval: float = DEFAULT_READINESS_POLL_INTERVAL


def _validate_config(config: GatewayAttachmentConfig) -> None:
    if not isinstance(config.profile_home, str) or not config.profile_home.strip():
        msg = 'profile_home must be a nonblank path'
        raise ValueError(msg)
    if not 0 <= config.readiness_timeout < float('inf'):
        msg = 'readiness_timeout must be finite and nonnegative'
        raise ValueError(msg)
    if not 0 < config.poll_interval < float('inf'):
        msg = 'poll_interval must be finite and positive'
        raise ValueError(msg)


async def _ready_endpoint(config: GatewayAttachmentConfig) -> object:
    deadline = time.monotonic() + config.readiness_timeout
    while True:
        try:
            discovery = await asyncio.to_thread(config.discover, config.profile_home)
            endpoint = getattr(discovery, 'endpoint', None)
            if endpoint is not None and config.is_attachable(discovery):
                return endpoint
        except Exception as exc:
            msg = 'Existing gateway discovery failed; no start attempted'
            raise GatewayAttachmentError(msg) from exc

        state = getattr(discovery, 'state', None)
        if state not in _TRANSIENT_STATES:
            msg = 'No attachable existing gateway; no start attempted'
            raise GatewayAttachmentError(msg)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            msg = 'Existing gateway readiness timed out; no start attempted'
            raise GatewayAttachmentError(msg)
        await asyncio.sleep(min(config.poll_interval, remaining))


@asynccontextmanager
async def attach_existing_gateway(config: GatewayAttachmentConfig) -> AsyncIterator[GatewayRPC]:
    """Wait for an *existing* gateway; never start, reconnect, or resubmit.

    Discovery retries are limited to transient ``absent`` / ``starting`` states.
    Only the single discovered ready endpoint is given to the injected connector.
    """
    _validate_config(config)
    endpoint = await _ready_endpoint(config)
    async with config.connector(endpoint=endpoint) as rpc:
        yield rpc


# Temporary exports for legacy module import compatibility.
__all__ = [
    'DEFAULT_READINESS_POLL_INTERVAL',
    'DEFAULT_READINESS_TIMEOUT',
    '_TRANSIENT_STATES',
    'Connector',
    'DiscoverGateway',
    'DiscoveryResult',
    'GatewayAttachmentConfig',
    'GatewayAttachmentError',
    'GatewayRPC',
    'IsAttachable',
    'Protocol',
    '_ready_endpoint',
    '_validate_config',
    'asynccontextmanager',
    'asyncio',
    'attach_existing_gateway',
    'dataclass',
    'time',
]
