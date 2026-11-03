from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.gateway.gateway_attachment import attach_existing_gateway
from hermes_harness.gateway.gateway_readonly import GatewayReadError, read_session

"""Read-only validation of an already-running Hermes gateway RPC contract.

Only ping, session.list, session.info and session.events.since are permitted.
No runtime is started and no session is created or resumed.
"""


if TYPE_CHECKING:
    from hermes_harness.gateway.gateway_attachment import GatewayAttachmentConfig, GatewayRPC


@dataclass(frozen=True, slots=True)
class GatewayContractReport:
    """Non-sensitive shape-only validation result; no session data is retained."""

    ping_verified: bool
    sampled_live_session_count: int
    info_verified: bool
    replay_epoch_present: bool
    replay_latest_seq: int | None
    replay_consistent: bool = False


def _check_ping(response: object) -> None:
    if not isinstance(response, dict) or response.get('pong') is not True:
        msg = 'Gateway ping response does not match Hermes contract'
        raise GatewayReadError(msg)


def _count_sessions(response: object) -> int:
    if not isinstance(response, dict) or response.get('scope') != 'live':
        msg = 'Gateway session.list returned an invalid scope'
        raise GatewayReadError(msg)
    sessions = response.get('sessions')
    if not isinstance(sessions, list) or any(not isinstance(item, dict) for item in sessions):
        msg = 'Gateway session.list returned an invalid sessions collection'
        raise GatewayReadError(msg)
    return len(sessions)


def _replay_metadata(response: object) -> tuple[str, int]:
    """Validate a replay envelope without retaining or exporting event payloads."""
    if not isinstance(response, dict):
        msg = 'Gateway replay metadata must be an object'
        raise GatewayReadError(msg)
    epoch = response.get('replay_epoch')
    latest = response.get('latest_seq')
    if not isinstance(epoch, str) or not epoch.strip():
        msg = 'Gateway replay epoch is missing'
        raise GatewayReadError(msg)
    if type(latest) is not int or latest < 0:
        msg = 'Gateway replay sequence is invalid'
        raise GatewayReadError(msg)
    if response.get('epoch') != epoch:
        msg = 'Gateway replay epoch fields disagree'
        raise GatewayReadError(msg)
    if type(response.get('truncated')) is not bool:
        msg = 'Gateway replay truncation flag is invalid'
        raise GatewayReadError(msg)
    if type(response.get('snapshot_required')) is not bool:
        msg = 'Gateway replay snapshot flag is invalid'
        raise GatewayReadError(msg)
    if not isinstance(response.get('events'), list):
        msg = 'Gateway replay events collection is invalid'
        raise GatewayReadError(msg)
    return epoch, latest


def _check_replay_watermark(response: object, *, epoch: str, latest: int) -> bool:
    """Check the same-epoch watermark without examining any event contents."""
    observed_epoch, observed_latest = _replay_metadata(response)
    if observed_epoch != epoch or observed_latest < latest:
        msg = 'Gateway replay epoch or sequence changed during validation'
        raise GatewayReadError(msg)
    if not isinstance(response, dict):
        msg = 'Gateway replay metadata must be an object'
        raise GatewayReadError(msg)
    if response['truncated'] or response['snapshot_required']:
        msg = 'Gateway replay requires a fresh snapshot'
        raise GatewayReadError(msg)
    # The second request starts at the first response's latest sequence.
    # New events may arrive concurrently; do not assert an empty result.
    return True


async def _validate_rpc(rpc: GatewayRPC, session_id: str | None) -> GatewayContractReport:
    """Perform bounded, single-shot reads without printing payloads or retrying."""
    _check_ping(await rpc.rpc('ping'))
    count = _count_sessions(await rpc.rpc('session.list', limit=1, scope='live'))
    if session_id is None:
        return GatewayContractReport(
            ping_verified=True,
            sampled_live_session_count=count,
            info_verified=False,
            replay_epoch_present=False,
            replay_latest_seq=None,
        )
    await read_session(rpc, session_id)
    metadata = await rpc.rpc('session.events.since', session_id=session_id, last_sequence=0)
    epoch, latest = _replay_metadata(metadata)
    verified = await rpc.rpc(
        'session.events.since',
        session_id=session_id,
        last_sequence=latest,
        replay_epoch=epoch,
    )
    consistent = _check_replay_watermark(verified, epoch=epoch, latest=latest)
    return GatewayContractReport(
        ping_verified=True,
        sampled_live_session_count=count,
        info_verified=True,
        replay_epoch_present=True,
        replay_latest_seq=latest,
        replay_consistent=consistent,
    )


async def validate_existing_gateway(
    config: GatewayAttachmentConfig, session_id: str | None = None
) -> GatewayContractReport:
    """Inspect an existing gateway, optionally checking one *known* live session.

    Caller injects Hermes discover_gateway_endpoint, attachable, and connect_gateway.
    Never select a session from the listing automatically. Do not log session metadata.
    """
    if session_id is not None and (not isinstance(session_id, str) or not session_id.strip()):
        msg = 'session_id must be a nonblank string'
        raise ValueError(msg)
    async with attach_existing_gateway(config) as rpc:
        return await _validate_rpc(rpc, session_id)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'GatewayContractReport',
    'GatewayReadError',
    '_check_ping',
    '_check_replay_watermark',
    '_count_sessions',
    '_replay_metadata',
    '_validate_rpc',
    'attach_existing_gateway',
    'dataclass',
    'read_session',
    'validate_existing_gateway',
]
