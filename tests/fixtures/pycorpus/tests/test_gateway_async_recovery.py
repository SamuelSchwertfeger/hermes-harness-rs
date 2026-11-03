"""Offline tests for read-only recovery through an injected async RPC."""

import asyncio
from typing import TYPE_CHECKING

import pytest

from hermes_harness.core.reconciliation import ReconciliationStatus
from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_async_recovery import recover_with_async_rpc
from hermes_harness.gateway.gateway_identity import GatewayContractError, Receipt, Submission
from hermes_harness.gateway.gateway_readonly import GatewayReadError

if TYPE_CHECKING:
    from pathlib import Path


class FakeRPC:
    def __init__(self, pending: object, *, receipt_id: str = 'adm-1', fail: str = '') -> None:
        self.pending = pending
        self.receipt_id = receipt_id
        self.fail = fail
        self.calls: list[tuple[str, dict[str, object]]] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        if self.fail == method:
            msg = 'simulated disconnect'
            raise OSError(msg)
        if method == 'session.events.since':
            if 'replay_epoch' not in params:
                return {
                    'events': [],
                    'latest_seq': 1,
                    'last_sequence': 1,
                    'epoch': 'epoch-1',
                    'replay_epoch': 'epoch-1',
                    'truncated': True,
                    'snapshot_required': True,
                    'count': 0,
                }
            return {
                'events': [
                    {
                        'type': 'session.info',
                        'session_id': 'session-1',
                        'payload': {'pending': self.pending},
                        'seq': 1,
                        'replay_epoch': 'epoch-1',
                    }
                ],
                'latest_seq': 1,
                'last_sequence': 1,
                'epoch': 'epoch-1',
                'replay_epoch': 'epoch-1',
                'truncated': False,
                'snapshot_required': False,
                'count': 1,
            }
        if method == 'prompt.receipt':
            return {'admission_id': self.receipt_id, 'status': 'queued'}
        msg = 'unexpected method'
        raise AssertionError(msg)


def unknown_store(tmp_path: Path, *, known: bool = False) -> AdmissionStore:
    store = AdmissionStore(tmp_path / 'admissions.sqlite')
    store.prepare(Submission('session-1', 'sub-1'))
    if known:
        store.record_receipt('sub-1', Receipt('adm-1', 'queued'))
    store.mark_outcome_unknown('sub-1')
    return store


def test_unique_pending_candidate_is_receipt_verified(tmp_path: Path) -> None:
    store = unknown_store(tmp_path)
    rpc = FakeRPC([{'input_id': 'sub-1', 'admission_id': 'adm-1'}])
    status = asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))
    assert status is ReconciliationStatus.RECEIPT_RECORDED
    record = store.get('sub-1')
    assert record is not None
    assert record.admission_id == 'adm-1'
    assert record.local_state is not LocalState.OUTCOME_UNKNOWN
    assert [method for method, _ in rpc.calls] == [
        'session.events.since',
        'session.events.since',
        'prompt.receipt',
    ]


@pytest.mark.parametrize(
    'pending',
    [
        [],
        None,
        'bad',
        [
            {'input_id': 'sub-1', 'admission_id': 'adm-1'},
            {'input_id': 'sub-1', 'admission_id': 'adm-2'},
        ],
    ],
)
def test_no_unambiguous_pending_match_stays_unknown(tmp_path: Path, pending: object) -> None:
    store = unknown_store(tmp_path)
    rpc = FakeRPC(pending)
    status = asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))
    assert status is ReconciliationStatus.STILL_UNKNOWN
    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert record.admission_id is None
    assert [method for method, _ in rpc.calls] == ['session.events.since', 'session.events.since']


def test_known_id_skips_event_replay(tmp_path: Path) -> None:
    store = unknown_store(tmp_path, known=True)
    rpc = FakeRPC(None)
    result = asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))
    assert result is ReconciliationStatus.RECEIPT_RECORDED
    assert [method for method, _ in rpc.calls] == ['prompt.receipt']


def test_receipt_identity_mismatch_leaves_known_id_and_unknown_state(tmp_path: Path) -> None:
    store = unknown_store(tmp_path, known=True)
    rpc = FakeRPC(None, receipt_id='other')
    with pytest.raises(GatewayContractError):
        asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))
    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert record.admission_id == 'adm-1'


@pytest.mark.parametrize('failed_method', ['session.events.since', 'prompt.receipt'])
def test_disconnect_retains_unknown(tmp_path: Path, failed_method: str) -> None:
    store = unknown_store(tmp_path, known=failed_method == 'prompt.receipt')
    rpc = FakeRPC([{'input_id': 'sub-1', 'admission_id': 'adm-1'}], fail=failed_method)
    exception_type = GatewayReadError if failed_method == 'session.events.since' else OSError
    with pytest.raises(exception_type):
        asyncio.run(recover_with_async_rpc(store, 'sub-1', rpc=rpc))
    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN


def test_ineligible_record_does_not_make_any_rpcs(tmp_path: Path) -> None:
    store = AdmissionStore(tmp_path / 'admissions.sqlite')
    rpc = FakeRPC([])
    result = asyncio.run(recover_with_async_rpc(store, 'missing', rpc=rpc))
    assert result is ReconciliationStatus.NOT_ELIGIBLE
    assert rpc.calls == []
