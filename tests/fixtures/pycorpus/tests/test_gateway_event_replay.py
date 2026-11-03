"""Offline tests for complete, epoch-bound pending-admission event replay."""

import asyncio
from dataclasses import dataclass

import pytest

from hermes_harness.gateway.gateway_event_replay import read_pending_snapshot
from hermes_harness.gateway.gateway_readonly import GatewayReadError

EXPECTED_REPLAY_CALLS = 2


@dataclass(frozen=True, slots=True)
class ReplayFixture:
    events: object
    epoch: str = 'epoch-1'
    latest_seq: int = 1
    truncated: bool = False
    fail_on_call: int | None = None
    malformed_initial: bool = False


class FakeReplayRPC:
    def __init__(self, fixture: ReplayFixture) -> None:
        self.fixture = fixture
        self.calls: list[tuple[str, dict[str, object]]] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        if self.fixture.fail_on_call == len(self.calls):
            msg = 'simulated disconnect'
            raise OSError(msg)
        if method != 'session.events.since':
            msg = f'unexpected method: {method}'
            raise AssertionError(msg)
        if 'replay_epoch' not in params:
            if self.fixture.malformed_initial:
                return {'replay_epoch': None}
            return {
                'events': [],
                'latest_seq': self.fixture.latest_seq,
                'last_sequence': self.fixture.latest_seq,
                'epoch': self.fixture.epoch,
                'replay_epoch': self.fixture.epoch,
                'truncated': True,
                'snapshot_required': True,
                'count': 0,
            }
        return {
            'events': self.fixture.events,
            'latest_seq': self.fixture.latest_seq,
            'last_sequence': self.fixture.latest_seq,
            'epoch': self.fixture.epoch,
            'replay_epoch': self.fixture.epoch,
            'truncated': self.fixture.truncated,
            'snapshot_required': self.fixture.truncated,
            'count': len(self.fixture.events) if isinstance(self.fixture.events, list) else 0,
        }


def test_complete_replay_returns_latest_pending_snapshot() -> None:
    rpc = FakeReplayRPC(
        ReplayFixture([
            {
                'type': 'session.info',
                'session_id': 'session-1',
                'payload': {'pending': [{'input_id': 'sub-1', 'admission_id': 'adm-1'}]},
                'seq': 1,
                'replay_epoch': 'epoch-1',
            }
        ])
    )

    snapshot = asyncio.run(read_pending_snapshot(rpc, 'session-1'))

    assert snapshot is not None
    assert snapshot.session_id == 'session-1'
    assert snapshot.data['pending'] == [{'input_id': 'sub-1', 'admission_id': 'adm-1'}]
    assert len(rpc.calls) == EXPECTED_REPLAY_CALLS
    assert rpc.calls[0][1] == {'session_id': 'session-1', 'last_sequence': 0}
    assert rpc.calls[1][1] == {
        'session_id': 'session-1',
        'last_sequence': 0,
        'replay_epoch': 'epoch-1',
    }


def test_truncated_replay_returns_no_candidate() -> None:
    rpc = FakeReplayRPC(
        ReplayFixture(
            [
                {
                    'type': 'session.info',
                    'session_id': 'session-1',
                    'payload': {'pending': []},
                    'seq': 1,
                }
            ],
            truncated=True,
        )
    )
    assert asyncio.run(read_pending_snapshot(rpc, 'session-1')) is None


def test_epoch_change_returns_no_candidate() -> None:
    rpc = FakeReplayRPC(
        ReplayFixture([
            {
                'type': 'session.info',
                'session_id': 'session-1',
                'payload': {'pending': []},
                'seq': 1,
            }
        ])
    )
    original_rpc = rpc.rpc
    calls = 0
    fixture = rpc.fixture

    async def race_rpc(method: str, **params: object) -> object:
        nonlocal calls
        calls += 1
        result = await original_rpc(method, **params)
        if calls == 1:
            rpc.fixture = ReplayFixture(
                events=fixture.events,
                epoch='epoch-2',
                latest_seq=fixture.latest_seq,
                truncated=fixture.truncated,
            )
        return result

    rpc.rpc = race_rpc  # type: ignore[method-assign]
    assert asyncio.run(read_pending_snapshot(rpc, 'session-1')) is None


def test_missing_pending_event_returns_no_candidate() -> None:
    rpc = FakeReplayRPC(
        ReplayFixture([
            {
                'type': 'message.complete',
                'session_id': 'session-1',
                'payload': {},
                'seq': 1,
                'replay_epoch': 'epoch-1',
            }
        ])
    )
    assert asyncio.run(read_pending_snapshot(rpc, 'session-1')) is None


def test_malformed_replay_envelope_is_rejected() -> None:
    rpc = FakeReplayRPC(ReplayFixture([], malformed_initial=True))
    with pytest.raises(GatewayReadError):
        asyncio.run(read_pending_snapshot(rpc, 'session-1'))


def test_disconnect_during_replay_is_not_retried() -> None:
    rpc = FakeReplayRPC(ReplayFixture([], fail_on_call=2))
    with pytest.raises(GatewayReadError, match='no retry'):
        asyncio.run(read_pending_snapshot(rpc, 'session-1'))
    assert len(rpc.calls) == EXPECTED_REPLAY_CALLS


def test_malformed_event_is_rejected() -> None:
    rpc = FakeReplayRPC(
        ReplayFixture([
            {'type': 'session.info', 'session_id': 'session-1', 'payload': {}, 'seq': 'bad'}
        ])
    )
    with pytest.raises(GatewayReadError):
        asyncio.run(read_pending_snapshot(rpc, 'session-1'))


def test_blank_session_id_is_rejected_without_rpc() -> None:
    rpc = FakeReplayRPC(ReplayFixture([]))
    with pytest.raises(ValueError, match='session_id'):
        asyncio.run(read_pending_snapshot(rpc, '  '))
    assert rpc.calls == []


def test_sequence_gap_returns_no_candidate() -> None:
    rpc = FakeReplayRPC(
        ReplayFixture(
            [
                {
                    'type': 'session.info',
                    'session_id': 'session-1',
                    'payload': {'pending': []},
                    'seq': 2,
                    'replay_epoch': 'epoch-1',
                }
            ],
            latest_seq=2,
        )
    )
    assert asyncio.run(read_pending_snapshot(rpc, 'session-1')) is None


def test_empty_event_history_does_not_request_replay() -> None:
    rpc = FakeReplayRPC(ReplayFixture([], latest_seq=0))
    assert asyncio.run(read_pending_snapshot(rpc, 'session-1')) is None
    assert len(rpc.calls) == 1
