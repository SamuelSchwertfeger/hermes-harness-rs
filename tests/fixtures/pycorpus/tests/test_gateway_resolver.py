"""Tests for the conservative known-admission receipt resolver."""

from pathlib import Path  # noqa: TC003 - runtime annotations on Python 3.13

import pytest

from hermes_harness.core.reconciliation import (
    LookupDisposition,
    ReconciliationStatus,
    reconcile_unknown,
)
from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_identity import GatewayContractError, Receipt, Submission
from hermes_harness.gateway.gateway_resolver import KnownAdmissionReceiptResolver


class FakeRPC:
    """Scripted RPC transport; no gateway or network is started."""

    def __init__(self, response: object = None, error: OSError | None = None) -> None:
        self.response = response
        self.error = error
        self.calls: list[tuple[str, dict[str, str]]] = []

    def call(self, method: str, params: dict[str, str]) -> object:
        self.calls.append((method, params))
        if self.error is not None:
            raise self.error
        return self.response


def make_unknown_store(tmp_path: Path, *, known_admission: bool) -> AdmissionStore:
    store = AdmissionStore(tmp_path / 'private' / 'admissions.sqlite3')
    store.prepare(Submission(session_id='session-1', submission_id='sub-1'))
    if known_admission:
        store.record_receipt('sub-1', Receipt('adm-1', 'queued'))
    store.mark_outcome_unknown('sub-1')
    return store


def test_unknown_admission_id_does_not_make_rpc_call(tmp_path: Path) -> None:
    store = make_unknown_store(tmp_path, known_admission=False)
    record = store.get('sub-1')
    assert record is not None
    rpc = FakeRPC({'admission_id': 'adm-1', 'status': 'queued'})

    result = KnownAdmissionReceiptResolver(rpc).resolve(record)

    assert result.disposition is LookupDisposition.INDETERMINATE
    assert rpc.calls == []


def test_known_admission_uses_receipt_rpc_and_validates_identity(tmp_path: Path) -> None:
    store = make_unknown_store(tmp_path, known_admission=True)
    record = store.get('sub-1')
    assert record is not None
    rpc = FakeRPC({'admission_id': 'adm-1', 'status': 'running', 'execution_generation': 3})

    result = KnownAdmissionReceiptResolver(rpc).resolve(record)

    assert result.disposition is LookupDisposition.FOUND
    assert result.receipt == Receipt('adm-1', 'running', 3)
    assert rpc.calls == [('prompt.receipt', {'session_id': 'session-1', 'admission_id': 'adm-1'})]


def test_changed_admission_identity_is_rejected(tmp_path: Path) -> None:
    store = make_unknown_store(tmp_path, known_admission=True)
    record = store.get('sub-1')
    assert record is not None
    resolver = KnownAdmissionReceiptResolver(FakeRPC({'admission_id': 'other', 'status': 'queued'}))

    with pytest.raises(GatewayContractError, match='changed admission identity'):
        resolver.resolve(record)


def test_rpc_disconnect_preserves_unknown_record(tmp_path: Path) -> None:
    store = make_unknown_store(tmp_path, known_admission=True)
    resolver = KnownAdmissionReceiptResolver(FakeRPC(error=OSError('simulated disconnect')))

    with pytest.raises(OSError, match='simulated disconnect'):
        reconcile_unknown(store, 'sub-1', resolver=resolver)

    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert record.admission_id == 'adm-1'


def test_known_receipt_can_be_reconciled_without_resubmission(tmp_path: Path) -> None:
    store = make_unknown_store(tmp_path, known_admission=True)
    rpc = FakeRPC({'admission_id': 'adm-1', 'status': 'queued'})

    result = reconcile_unknown(store, 'sub-1', resolver=KnownAdmissionReceiptResolver(rpc))

    assert result is ReconciliationStatus.RECEIPT_RECORDED
    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.RECEIPT_OBSERVED
    assert record.admission_id == 'adm-1'
    assert len(rpc.calls) == 1
