"""Read-only session inspection through an explicitly injected authenticated connector.

The harness neither imports Hermes nor discovers or starts a gateway. The caller
must provide a connector that attaches to an existing, authenticated endpoint.
"""

from contextlib import AbstractAsyncContextManager
from dataclasses import dataclass
from typing import Protocol


class GatewayReadError(RuntimeError):
    """Gateway read was refused, disconnected, or returned an invalid payload."""


@dataclass(frozen=True, slots=True)
class SessionSnapshot:
    """A bounded, opaque response returned by ``session.info``."""

    session_id: str
    data: dict[str, object]


class GatewayRPC(Protocol):
    """Minimal already-authenticated RPC interface."""

    async def rpc(self, method: str, **params: object) -> object:
        """Make a gateway RPC request."""
        ...


class GatewayConnector(Protocol):
    """Supplied by an authenticated host; must not auto-start a gateway."""

    def __call__(self, *, endpoint: object) -> AbstractAsyncContextManager[GatewayRPC]:
        """Open a connection to the previously discovered endpoint."""
        ...


def _nonblank(value: str, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        msg = f'{label} must be a nonblank string'
        raise ValueError(msg)
    return value


async def read_session(rpc: GatewayRPC, session_id: str) -> SessionSnapshot:
    """Read metadata using an already-authenticated RPC connection; never retry."""
    session_id = _nonblank(session_id, 'session_id')
    try:
        response = await rpc.rpc('session.info', session_id=session_id)
    except OSError as exc:
        msg = 'Gateway session read failed; no retry was attempted'
        raise GatewayReadError(msg) from exc
    if not isinstance(response, dict) or any(not isinstance(key, str) for key in response):
        msg = 'session.info must return an object with string keys'
        raise GatewayReadError(msg)
    return SessionSnapshot(session_id=session_id, data=response)


async def read_from_hermes(
    endpoint: object,
    session_id: str,
    *,
    connector: GatewayConnector,
) -> SessionSnapshot:
    """Use a caller-provided authenticated connector with an explicit endpoint.

    A Hermes-side caller can pass ``hermes_cli.gateway_client.connect_gateway``
    after explicitly discovering and validating a ready endpoint. This module
    never imports Hermes, consults credentials, or starts the gateway itself.
    """
    if endpoint is None:
        msg = 'A pre-discovered gateway endpoint is mandatory; auto-start is forbidden'
        raise ValueError(msg)
    _nonblank(session_id, 'session_id')
    async with connector(endpoint=endpoint) as rpc:
        return await read_session(rpc, session_id)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'AbstractAsyncContextManager',
    'GatewayConnector',
    'GatewayRPC',
    'GatewayReadError',
    'Protocol',
    'SessionSnapshot',
    '_nonblank',
    'dataclass',
    'read_from_hermes',
    'read_session',
]
