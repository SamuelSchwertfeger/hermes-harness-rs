import asyncio
from typing import TYPE_CHECKING

import pytest

from hermes_harness.core.reconciliation import ReconciliationStatus
from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_async_recovery import recover_with_async_rpc
from hermes_harness.gateway.gateway_identity import GatewayContractError, Receipt, Submission
from hermes_harness.gateway.gateway_readonly import GatewayReadError

MAX_REPLAY_CALLS = 2

"""Adversarial, offline identity and transport matrix for gateway receipt recovery.

No Hermes process or network connection is used; every RPC is injected.
"""


if TYPE_CHECKING:
    from pathlib import Path


class ScriptedRPC:
    """Two-pass event replay followed by one authoritative receipt read."""

    def __init__(
        self, *, receipt: object = None, fail_on: str = '', pending_id: str = 'sub-1'
    ) -> None:
        self.receipt = receipt
        self.fail_on = fail_on
        self.pending_id = pending_id
        self.calls: list[tuple[str, dict[str, object]]] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        if method == self.fail_on:
            msg = 'connection interrupted'
            raise OSError(msg)
        if method == 'session.events.since':
            return self._replay(verified='replay_epoch' in params)
        if method == 'prompt.receipt':
            return self.receipt
        msg = 'unexpected RPC: no submission is authorized'
        raise RuntimeError(msg)

    def _replay(self, *, verified: bool) -> dict[str, object]:
        event = {
            'type': 'session.info',
            'session_id': 'session-1',
            'seq': 1,
            'replay_epoch': 'epoch-1',
            'payload': {'pending': [{'input_id': self.pending_id, 'admission_id': 'adm-1'}]},
        }
        return {
            'events': [event] if verified else [],
            'epoch': 'epoch-1',
            'replay_epoch': 'epoch-1',
            'latest_seq': 1,
            'truncated': not verified,
            'snapshot_required': not verified,
        }


def unknown_ledger(tmp_path: Path, *, known: bool = False) -> AdmissionStore:
    """Create a disposable, uncertain admission with distinct identity fields."""
    store = AdmissionStore(tmp_path / 'admissions.sqlite')
    store.prepare(Submission(session_id='session-1', submission_id='sub-1'))
    if known:
        store.record_receipt('sub-1', Receipt('adm-1', 'queued'))
    store.mark_outcome_unknown('sub-1')
    return store


def assert_safe_calls(rpc: ScriptedRPC) -> None:
    """No path may submit, cancel, resume, or make a repeated receipt read."""
    methods = [method for method, _ in rpc.calls]
    assert set(methods) <= {'session.events.since', 'prompt.receipt'}
    assert methods.count('prompt.receipt') <= 1
    assert methods.count('session.events.since') <= MAX_REPLAY_CALLS


@pytest.mark.parametrize('known', [False, True])
@pytest.mark.parametrize('status', ['queued', 'running', 'complete', 'unrecognized'])
def test_authoritative_receipt_matches_admission_not_submission(
    tmp_path: Path, *, known: bool, status: str
) -> None:
    store = unknown_ledger(tmp_path, known=known)
    rpc = ScriptedRPC(receipt={'admission_id': 'adm-1', 'status': status})

    outcome = asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))

    assert outcome is ReconciliationStatus.RECEIPT_RECORDED
    record = store.get('sub-1')
    assert record is not None
    assert record.submission_id == 'sub-1'
    assert record.admission_id == 'adm-1'
    assert record.gateway_status == status
    assert record.local_state is LocalState.RECEIPT_OBSERVED
    assert_safe_calls(rpc)
    if known:
        assert [method for method, _ in rpc.calls] == ['prompt.receipt']
    else:
        assert [method for method, _ in rpc.calls] == [
            'session.events.since',
            'session.events.since',
            'prompt.receipt',
        ]
    assert rpc.calls[-1][1]['admission_id'] == 'adm-1'
    assert rpc.calls[-1][1]['session_id'] == 'session-1'


@pytest.mark.parametrize('known', [False, True])
@pytest.mark.parametrize(
    'invalid_receipt',
    [
        {'admission_id': 'sub-1', 'status': 'queued'},
        {'admission_id': 'adm-2', 'status': 'queued'},
        {'admission_id': 'adm-1'},
        {'admission_id': 'adm-1', 'status': ''},
        {'admission_id': 'adm-1', 'status': 'queued', 'execution_generation': True},
        [],
    ],
)
def test_untrusted_receipt_never_reconciles(
    tmp_path: Path, *, known: bool, invalid_receipt: object
) -> None:
    store = unknown_ledger(tmp_path, known=known)
    before = store.get('sub-1')
    rpc = ScriptedRPC(receipt=invalid_receipt)

    with pytest.raises(GatewayContractError, match='Gateway'):
        asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))

    after = store.get('sub-1')
    assert after is not None
    assert before is not None
    assert after.local_state is LocalState.OUTCOME_UNKNOWN
    assert after.admission_id == before.admission_id
    assert after.gateway_status == before.gateway_status
    assert_safe_calls(rpc)


@pytest.mark.parametrize('known', [False, True])
def test_receipt_transport_interruption_preserves_identity(tmp_path: Path, *, known: bool) -> None:
    store = unknown_ledger(tmp_path, known=known)
    before = store.get('sub-1')
    rpc = ScriptedRPC(fail_on='prompt.receipt')

    with pytest.raises(OSError, match='connection interrupted'):
        asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))

    after = store.get('sub-1')
    assert after is not None
    assert before is not None
    assert after.local_state is LocalState.OUTCOME_UNKNOWN
    assert after.admission_id == before.admission_id
    assert_safe_calls(rpc)


def test_event_replay_disconnect_never_attempts_receipt(tmp_path: Path) -> None:
    store = unknown_ledger(tmp_path)
    rpc = ScriptedRPC(fail_on='session.events.since')

    with pytest.raises(GatewayReadError, match='no retry'):
        asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))

    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert record.admission_id is None
    assert [method for method, _ in rpc.calls] == ['session.events.since']
    assert_safe_calls(rpc)


def test_unrelated_pending_identity_is_not_authority(tmp_path: Path) -> None:
    store = unknown_ledger(tmp_path)
    rpc = ScriptedRPC(pending_id='other-submission')

    outcome = asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))

    assert outcome is ReconciliationStatus.STILL_UNKNOWN
    record = store.get('sub-1')
    assert record is not None
    assert record.admission_id is None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert [method for method, _ in rpc.calls] == ['session.events.since', 'session.events.since']
    assert_safe_calls(rpc)
