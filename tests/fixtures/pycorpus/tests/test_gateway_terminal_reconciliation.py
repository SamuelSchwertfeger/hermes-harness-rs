"""Terminal receipt reconciliation never submits, retries, or lies about outcomes."""

import asyncio
from typing import TYPE_CHECKING

import pytest

from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_identity import GatewayContractError, Receipt, Submission
from hermes_harness.gateway.gateway_terminal_reconciliation import (
    TerminalReconciliationError,
    reconcile_terminal_once,
)

SESSION = 'local-example'
SUBMISSION = 'submission-1'
ADMISSION = 'admission-1'

if TYPE_CHECKING:
    from pathlib import Path


class FakeRPC:
    def __init__(self, result: object) -> None:
        self.result = result
        self.calls: list[tuple[str, dict[str, object]]] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        if isinstance(self.result, Exception):
            raise self.result
        return self.result


def store(tmp_path: Path) -> AdmissionStore:
    db = AdmissionStore(tmp_path / 'admissions.sqlite')
    db.prepare(Submission(SESSION, SUBMISSION))
    db.record_receipt(SUBMISSION, Receipt(ADMISSION, 'queued'))
    return db


def receipt(outcome: str = 'completed') -> dict[str, object]:
    return {
        'admission_id': ADMISSION,
        'ref': {'session_id': SESSION, 'profile_id': '/home/hermes/.hermes'},
        'status': 'terminal',
        'outcome': outcome,
        'execution_generation': 1,
        'result': {'completed': outcome == 'completed', 'final_response': 'M3_OK'},
    }


def test_completed_result_is_durable(tmp_path: Path) -> None:
    db = store(tmp_path)
    rpc = FakeRPC(receipt())
    result = asyncio.run(reconcile_terminal_once(rpc, db, SUBMISSION))
    assert result.outcome == 'completed'
    assert result.final_response == 'M3_OK'
    assert result.recorded.local_state is LocalState.RECEIPT_OBSERVED
    assert result.recorded.gateway_status == 'terminal'
    assert result.recorded.execution_generation == 1
    assert rpc.calls == [
        (
            'prompt.receipt',
            {
                'session_id': SESSION,
                'admission_id': ADMISSION,
                'include_result': True,
            },
        )
    ]


@pytest.mark.parametrize('outcome', ['failed', 'interrupted', 'cancelled'])
def test_other_terminal_outcomes_are_not_success(tmp_path: Path, outcome: str) -> None:
    db = store(tmp_path)
    rpc = FakeRPC(receipt(outcome))
    evidence = asyncio.run(reconcile_terminal_once(rpc, db, SUBMISSION))
    assert evidence.outcome == outcome
    assert evidence.recorded.gateway_status == 'terminal'


@pytest.mark.parametrize(
    'bad',
    [
        {'admission_id': ADMISSION, 'status': 'queued'},
        {'admission_id': ADMISSION, 'status': 'terminal', 'outcome': 'completed'},
        {'admission_id': ADMISSION, 'status': 'terminal', 'outcome': 'unknown'},
        dict(receipt(), admission_id='wrong'),
        dict(receipt(), ref={'session_id': 'wrong'}),
        dict(receipt(), result={'completed': False}),
    ],
)
def test_invalid_receipts_do_not_change_ledger(tmp_path: Path, bad: dict[str, object]) -> None:
    db = store(tmp_path)
    before = db.get(SUBMISSION)
    rpc = FakeRPC(bad)
    with pytest.raises((GatewayContractError, TerminalReconciliationError)):
        asyncio.run(reconcile_terminal_once(rpc, db, SUBMISSION))
    assert db.get(SUBMISSION) == before
    assert [name for name, _ in rpc.calls] == ['prompt.receipt']


def test_transport_failure_preserves_ledger(tmp_path: Path) -> None:
    db = store(tmp_path)
    before = db.get(SUBMISSION)
    message = 'offline'
    rpc = FakeRPC(ConnectionError(message))
    with pytest.raises(ConnectionError, match='offline'):
        asyncio.run(reconcile_terminal_once(rpc, db, SUBMISSION))
    assert db.get(SUBMISSION) == before
    assert len(rpc.calls) == 1


def test_missing_admission_never_contacts_rpc(tmp_path: Path) -> None:
    db = AdmissionStore(tmp_path / 'new.sqlite')
    rpc = FakeRPC(receipt())
    with pytest.raises(TerminalReconciliationError, match='Known durable'):
        asyncio.run(reconcile_terminal_once(rpc, db, SUBMISSION))
    assert rpc.calls == []
