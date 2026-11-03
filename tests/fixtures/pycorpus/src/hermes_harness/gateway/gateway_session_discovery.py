"""Enumerate authorized session identities through an existing Hermes gateway.

No session is selected, resumed, created, or modified. Only session.list is sent.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Literal

from hermes_harness.gateway.gateway_attachment import attach_existing_gateway
from hermes_harness.gateway.gateway_readonly import GatewayReadError

if TYPE_CHECKING:
    from hermes_harness.gateway.gateway_attachment import GatewayAttachmentConfig, GatewayRPC


SessionScope = Literal['live', 'stored']
MAX_SESSION_SAMPLE = 20


@dataclass(frozen=True, slots=True)
class SessionCandidate:
    """Only the identifier and running status, never transcript metadata."""

    session_id: str
    running: bool


@dataclass(frozen=True, slots=True)
class SessionDiscoveryReport:
    """Bounded view of sessions already accessible to the authenticated actor."""

    scope: SessionScope
    sessions: tuple[SessionCandidate, ...]
    limit: int


def _parse_listing(response: object, *, scope: SessionScope, limit: int) -> SessionDiscoveryReport:
    if not isinstance(response, dict) or response.get('scope') != scope:
        msg = 'Gateway session listing returned an unexpected scope'
        raise GatewayReadError(msg)
    rows = response.get('sessions')
    if not isinstance(rows, list) or len(rows) > limit:
        msg = 'Gateway session listing is malformed or exceeds its requested limit'
        raise GatewayReadError(msg)
    candidates: list[SessionCandidate] = []
    seen: set[str] = set()
    for row in rows:
        if not isinstance(row, dict):
            msg = 'Gateway session listing contains a malformed row'
            raise GatewayReadError(msg)
        session_id = row.get('session_id')
        running = row.get('running')
        if not isinstance(session_id, str) or not session_id.strip() or session_id in seen:
            msg = 'Gateway session listing contains an invalid or repeated identifier'
            raise GatewayReadError(msg)
        if type(running) is not bool:
            msg = 'Gateway session listing contains an invalid running flag'
            raise GatewayReadError(msg)
        seen.add(session_id)
        candidates.append(SessionCandidate(session_id=session_id, running=running))
    return SessionDiscoveryReport(scope=scope, sessions=tuple(candidates), limit=limit)


async def _list_sessions(
    rpc: GatewayRPC, *, scope: SessionScope, limit: int
) -> SessionDiscoveryReport:
    response = await rpc.rpc('session.list', scope=scope, limit=limit)
    return _parse_listing(response, scope=scope, limit=limit)


async def discover_existing_sessions(
    config: GatewayAttachmentConfig, *, scope: SessionScope = 'stored', limit: int = 10
) -> SessionDiscoveryReport:
    """List authorized sessions only; propagate RPC failures without retrying."""
    if scope not in ('live', 'stored'):
        msg = 'scope must be live or stored'
        raise ValueError(msg)
    if type(limit) is not int or not 1 <= limit <= MAX_SESSION_SAMPLE:
        msg = 'limit must be an integer from 1 through 20'
        raise ValueError(msg)
    async with attach_existing_gateway(config) as rpc:
        return await _list_sessions(rpc, scope=scope, limit=limit)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_SESSION_SAMPLE',
    'GatewayReadError',
    'Literal',
    'SessionCandidate',
    'SessionDiscoveryReport',
    'SessionScope',
    '_list_sessions',
    '_parse_listing',
    'attach_existing_gateway',
    'dataclass',
    'discover_existing_sessions',
]
