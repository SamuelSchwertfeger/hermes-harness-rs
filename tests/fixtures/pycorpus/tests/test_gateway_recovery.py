"""Offline tests for read-only uncertain-submission recovery."""

from typing import TYPE_CHECKING

import pytest

from hermes_harness.core.reconciliation import ReconciliationStatus
from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_identity import GatewayContractError, Receipt, Submission
from hermes_harness.gateway.gateway_recovery import RecoveryClients, recover_unknown_admission

if TYPE_CHECKING:
    from pathlib import Path


class ReadRPC:
    """Scripted read-only session RPC; fails if an unexpected method is used."""

    def __init__(self, pending: object, error: OSError | None = None) -> None:
        self.pending = pending
        self.error = error
        self.calls: list[tuple[str, dict[str, object]]] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        assert method == 'session.events.since'
        if self.error is not None:
            raise self.error
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


class ReceiptRPC:
    """Scripted known-admission receipt RPC; never accepts prompt.submit."""

    def __init__(self, response: object, error: OSError | None = None) -> None:
        self.response = response
        self.error = error
        self.calls: list[tuple[str, dict[str, str]]] = []

    def call(self, method: str, params: dict[str, str]) -> object:
        self.calls.append((method, params))
        assert method == 'prompt.receipt'
        if self.error is not None:
            raise self.error
        return self.response


def unknown_store(tmp_path: Path, *, admission_id: str | None = None) -> AdmissionStore:
    """Prepare a durable submission whose transport outcome is uncertain."""
    store = AdmissionStore(tmp_path / 'private' / 'admissions.sqlite3')
    store.prepare(Submission('session-1', 'sub-1'))
    if admission_id is not None:
        store.record_receipt('sub-1', Receipt(admission_id, 'queued'))
    store.mark_outcome_unknown('sub-1')
    return store


@pytest.mark.asyncio
async def test_unique_pending_candidate_is_receipt_verified(tmp_path: Path) -> None:
    store = unknown_store(tmp_path)
    reader = ReadRPC([{'input_id': 'sub-1', 'admission_id': 'adm-1'}])
    receipts = ReceiptRPC({'admission_id': 'adm-1', 'status': 'started', 'execution_generation': 2})

    result = await recover_unknown_admission(
        store, 'sub-1', clients=RecoveryClients(reader, receipts)
    )

    assert result is ReconciliationStatus.RECEIPT_RECORDED
    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.RECEIPT_OBSERVED
    assert record.admission_id == 'adm-1'
    assert record.gateway_status == 'started'
    assert [method for method, _ in reader.calls] == [
        'session.events.since',
        'session.events.since',
    ]
    assert receipts.calls == [
        ('prompt.receipt', {'session_id': 'session-1', 'admission_id': 'adm-1'})
    ]


@pytest.mark.parametrize(
    'pending',
    [
        [],
        None,
        'bad',
        [42],
        [
            {'input_id': 'sub-1', 'admission_id': 'a'},
            {'input_id': 'sub-1', 'admission_id': 'b'},
        ],
    ],
)
@pytest.mark.asyncio
async def test_no_unambiguous_pending_match_stays_unknown(tmp_path: Path, pending: object) -> None:
    store = unknown_store(tmp_path)
    reader = ReadRPC(pending)
    receipts = ReceiptRPC({'admission_id': 'a', 'status': 'queued'})

    result = await recover_unknown_admission(
        store, 'sub-1', clients=RecoveryClients(reader, receipts)
    )

    assert result is ReconciliationStatus.STILL_UNKNOWN
    assert receipts.calls == []
    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert record.admission_id is None


@pytest.mark.asyncio
async def test_known_id_skips_snapshot(tmp_path: Path) -> None:
    store = unknown_store(tmp_path, admission_id='adm-1')
    reader = ReadRPC([])
    receipts = ReceiptRPC({'admission_id': 'adm-1', 'status': 'queued'})

    result = await recover_unknown_admission(
        store, 'sub-1', clients=RecoveryClients(reader, receipts)
    )

    assert result is ReconciliationStatus.RECEIPT_RECORDED
    assert reader.calls == []
    assert len(receipts.calls) == 1


@pytest.mark.asyncio
async def test_mismatched_receipt_cannot_persist_candidate(tmp_path: Path) -> None:
    store = unknown_store(tmp_path)
    reader = ReadRPC([{'input_id': 'sub-1', 'admission_id': 'adm-1'}])
    receipts = ReceiptRPC({'admission_id': 'different', 'status': 'queued'})

    with pytest.raises(GatewayContractError, match='changed admission identity'):
        await recover_unknown_admission(store, 'sub-1', clients=RecoveryClients(reader, receipts))

    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert record.admission_id is None


@pytest.mark.asyncio
async def test_receipt_disconnect_retains_unknown(tmp_path: Path) -> None:
    store = unknown_store(tmp_path)
    reader = ReadRPC([{'input_id': 'sub-1', 'admission_id': 'adm-1'}])
    receipts = ReceiptRPC(None, error=OSError('disconnected'))

    with pytest.raises(OSError, match='disconnected'):
        await recover_unknown_admission(store, 'sub-1', clients=RecoveryClients(reader, receipts))

    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN


@pytest.mark.asyncio
async def test_read_disconnect_retains_unknown(tmp_path: Path) -> None:
    store = unknown_store(tmp_path)
    reader = ReadRPC([], error=OSError('disconnected'))
    receipts = ReceiptRPC(None)

    with pytest.raises(RuntimeError, match='event replay failed'):
        await recover_unknown_admission(store, 'sub-1', clients=RecoveryClients(reader, receipts))

    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert receipts.calls == []


@pytest.mark.asyncio
async def test_ineligible_does_not_make_any_rpcs(tmp_path: Path) -> None:
    store = AdmissionStore(tmp_path / 'admissions.sqlite3')
    store.prepare(Submission('session-1', 'sub-1'))
    reader = ReadRPC([])
    receipts = ReceiptRPC(None)

    result = await recover_unknown_admission(
        store, 'sub-1', clients=RecoveryClients(reader, receipts)
    )

    assert result is ReconciliationStatus.NOT_ELIGIBLE
    assert reader.calls == []
    assert receipts.calls == []
