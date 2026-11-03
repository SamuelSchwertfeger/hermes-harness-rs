"""Read a complete, epoch-bound replay to recover a pending-admission candidate.

Hermes' ``session.info`` RPC is a session metadata projection; the pending list is
published in ``session.info`` events. This module uses only the read-only
``session.events.since`` RPC and refuses truncated or inconsistent replays.
"""

from dataclasses import dataclass
from typing import Protocol

from hermes_harness.gateway.gateway_readonly import GatewayReadError, SessionSnapshot


class EventReplayRPC(Protocol):
    """An already-authenticated gateway RPC connection."""

    async def rpc(self, method: str, **params: object) -> object:
        """Perform one RPC without retrying or reconnecting."""
        ...


@dataclass(frozen=True, slots=True)
class _ReplayEnvelope:
    epoch: str
    latest_seq: int
    truncated: bool
    snapshot_required: bool
    events: list[object]


@dataclass(slots=True)
class _ReplayContext:
    session_id: str
    epoch: str
    latest_seq: int
    previous_seq: int = 0


def _replay_fields(response: object) -> _ReplayEnvelope:
    """Validate the stable envelope returned by ``session.events.since``."""
    if not isinstance(response, dict) or any(not isinstance(key, str) for key in response):
        msg = 'session.events.since must return an object with string keys'
        raise GatewayReadError(msg)

    epoch = response.get('replay_epoch')
    latest_seq = response.get('latest_seq')
    truncated = response.get('truncated')
    snapshot_required = response.get('snapshot_required')
    events = response.get('events')
    if not isinstance(epoch, str) or not epoch.strip() or response.get('epoch') != epoch:
        msg = 'session.events.since returned an invalid replay envelope'
        raise GatewayReadError(msg)
    if type(latest_seq) is not int or latest_seq < 0:
        msg = 'session.events.since returned an invalid replay envelope'
        raise GatewayReadError(msg)
    if type(truncated) is not bool or type(snapshot_required) is not bool:
        msg = 'session.events.since returned an invalid replay envelope'
        raise GatewayReadError(msg)
    if not isinstance(events, list):
        msg = 'session.events.since returned an invalid replay envelope'
        raise GatewayReadError(msg)
    return _ReplayEnvelope(epoch, latest_seq, truncated, snapshot_required, events)


def _event_fields(event: object, context: _ReplayContext) -> tuple[int, str, dict[object, object]]:
    """Validate one replay event and return its sequence, type, and payload."""
    if not isinstance(event, dict):
        msg = 'session.events.since returned a malformed event'
        raise GatewayReadError(msg)

    sequence = event.get('seq')
    event_type = event.get('type')
    payload = event.get('payload')
    if type(sequence) is not int:
        msg = 'session.events.since returned an inconsistent event'
        raise GatewayReadError(msg)
    if sequence <= context.previous_seq or sequence > context.latest_seq:
        msg = 'session.events.since returned an inconsistent event'
        raise GatewayReadError(msg)
    if event.get('session_id') != context.session_id or event.get('replay_epoch') != context.epoch:
        msg = 'session.events.since returned an inconsistent event'
        raise GatewayReadError(msg)
    if not isinstance(event_type, str) or not isinstance(payload, dict):
        msg = 'session.events.since returned an inconsistent event'
        raise GatewayReadError(msg)
    return sequence, event_type, payload


def _latest_pending(replay: _ReplayEnvelope, session_id: str) -> list[object] | None:
    """Return the newest pending snapshot, refusing malformed or absent snapshots."""
    latest: tuple[int, list[object]] | None = None
    context = _ReplayContext(session_id, replay.epoch, replay.latest_seq)
    for event in replay.events:
        sequence, event_type, payload = _event_fields(event, context)
        if sequence != context.previous_seq + 1:
            return None
        context.previous_seq = sequence
        if event_type != 'session.info' or 'pending' not in payload:
            continue
        pending = payload['pending']
        if not isinstance(pending, list):
            return None
        latest = (sequence, pending)
    if context.previous_seq != replay.latest_seq or latest is None:
        return None
    return latest[1]


async def read_pending_snapshot(rpc: EventReplayRPC, session_id: str) -> SessionSnapshot | None:
    """Return the latest replayed pending snapshot, or ``None`` when uncertain.

    The first request learns the current replay epoch. The second requests the
    entire retained history from sequence zero under that epoch. A gap, epoch
    change, or missing snapshot is inconclusive, never proof of non-admission.
    """
    if not isinstance(session_id, str) or not session_id.strip():
        msg = 'session_id must be a nonblank string'
        raise ValueError(msg)

    try:
        initial = _replay_fields(
            await rpc.rpc('session.events.since', session_id=session_id, last_sequence=0)
        )
        if initial.latest_seq == 0:
            return None
        replay = _replay_fields(
            await rpc.rpc(
                'session.events.since',
                session_id=session_id,
                last_sequence=0,
                replay_epoch=initial.epoch,
            )
        )
    except OSError as exc:
        msg = 'Gateway event replay failed; no retry was attempted'
        raise GatewayReadError(msg) from exc

    if replay.epoch != initial.epoch or replay.truncated or replay.snapshot_required:
        return None
    if replay.latest_seq < initial.latest_seq:
        return None
    pending = _latest_pending(replay, session_id)
    if pending is None:
        return None
    return SessionSnapshot(session_id=session_id, data={'pending': pending})


# Temporary exports for legacy module import compatibility.
__all__ = [
    'EventReplayRPC',
    'GatewayReadError',
    'Protocol',
    'SessionSnapshot',
    '_ReplayContext',
    '_ReplayEnvelope',
    '_event_fields',
    '_latest_pending',
    '_replay_fields',
    'dataclass',
    'read_pending_snapshot',
]
