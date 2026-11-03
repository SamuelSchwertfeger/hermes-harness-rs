"""Offline same-connection and fail-closed session-to-admission integration tests."""

import asyncio
import json
from pathlib import Path

import pytest

from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_disposable_admission import (
    DisposableAdmissionRequest,
    create_and_submit_once,
)
from hermes_harness.gateway.gateway_identity import GatewayOutcomeUnknownError
from hermes_harness.gateway.gateway_session_creation import (
    SessionCreationError,
    SessionCreationRequest,
    SessionCreationUnknownError,
)

SESSION_ID = 'local-aaaaaaaaaaaaaaaa'


class FakeRPC:
    """One shared connection that records all RPC activity and injected failures."""

    def __init__(self, *, failures: dict[str, object] | None = None) -> None:
        self.calls: list[tuple[str, dict[str, object]]] = []
        self.failures = failures or {}

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        response = self.failures.get(method)
        if isinstance(response, Exception):
            raise response
        if response is not None:
            return response
        if method == 'session.create':
            return {'session_id': SESSION_ID, 'subscription_id': 'sub-1'}
        if method == 'prompt.submit':
            return {'admission_id': 'adm-1', 'status': 'queued'}
        if method == 'prompt.receipt':
            return {'admission_id': 'adm-1', 'status': 'queued'}
        msg = f'Unexpected RPC: {method}'
        raise AssertionError(msg)


def make_request(tmp_path: Path) -> DisposableAdmissionRequest:
    return DisposableAdmissionRequest(
        creation=SessionCreationRequest('create-1', tmp_path / 'creation.json'),
        submission_id='submit-1',
        text='Reply with M3_OK. Do not use tools.',
    )


def make_store(tmp_path: Path) -> AdmissionStore:
    return AdmissionStore(tmp_path / 'admissions.sqlite')


def methods(rpc: FakeRPC) -> list[str]:
    return [method for method, _ in rpc.calls]


def test_order_and_same_rpc(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    request = make_request(tmp_path)
    rpc = FakeRPC()
    result = asyncio.run(create_and_submit_once(rpc, store, request))
    assert methods(rpc) == ['session.create', 'prompt.submit', 'prompt.receipt']
    assert rpc.calls[0][1]['request_id'] == 'create-1'
    assert rpc.calls[0][1]['source'] == 'cli'
    assert rpc.calls[0][1]['toolsets'] == []
    assert rpc.calls[0][1]['max_turns'] == 1
    expected_run_budget = 60
    assert rpc.calls[0][1]['run_budget'] == expected_run_budget
    assert Path(rpc.calls[0][1]['cwd']).is_dir()
    assert rpc.calls[1][1]['session_id'] == result.prepared.session_id
    assert rpc.calls[1][1]['submission_id'] == 'submit-1'
    assert rpc.calls[2][1]['admission_id'] == result.admission.admission_id
    assert result.prepared.subscription_id == 'sub-1'
    assert result.admission.admission_id == 'adm-1'
    record = store.get('submit-1')
    assert record is not None
    assert record.local_state is LocalState.ADMITTED
    assert json.loads(request.creation.record_path.read_text())['status'] == 'attached'


@pytest.mark.parametrize('response', [None, {}, {'session_id': SESSION_ID}])
def test_missing_attachment_never_submits(tmp_path: Path, response: object) -> None:
    store = make_store(tmp_path)
    rpc = FakeRPC(failures={'session.create': response if response is not None else []})
    with pytest.raises(SessionCreationUnknownError):
        asyncio.run(create_and_submit_once(rpc, store, make_request(tmp_path)))
    assert methods(rpc) == ['session.create']
    assert store.get('submit-1') is None


def test_disconnected_creation_never_submits_or_retries(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    request = make_request(tmp_path)
    rpc = FakeRPC(failures={'session.create': ConnectionError('disconnect')})
    with pytest.raises(SessionCreationUnknownError):
        asyncio.run(create_and_submit_once(rpc, store, request))
    with pytest.raises(SessionCreationError, match='already exists'):
        asyncio.run(create_and_submit_once(rpc, store, request))
    assert methods(rpc) == ['session.create']


def test_disconnected_submit_remains_unknown_and_cannot_repeat(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    request = make_request(tmp_path)
    rpc = FakeRPC(failures={'prompt.submit': ConnectionError('disconnect')})
    with pytest.raises(GatewayOutcomeUnknownError):
        asyncio.run(create_and_submit_once(rpc, store, request))
    record = store.get('submit-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    with pytest.raises(ValueError, match='already exists'):
        asyncio.run(create_and_submit_once(rpc, store, request))
    assert methods(rpc) == ['session.create', 'prompt.submit']


def test_receipt_failure_preserves_identity(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    rpc = FakeRPC(failures={'prompt.receipt': ConnectionError('disconnect')})
    with pytest.raises(ConnectionError):
        asyncio.run(create_and_submit_once(rpc, store, make_request(tmp_path)))
    record = store.get('submit-1')
    assert record is not None
    assert record.admission_id == 'adm-1'
    assert methods(rpc) == ['session.create', 'prompt.submit', 'prompt.receipt']


@pytest.mark.parametrize('submission_id', ['', ' '])
def test_invalid_submission_id_prevents_creation(tmp_path: Path, submission_id: str) -> None:
    store = make_store(tmp_path)
    rpc = FakeRPC()
    request = make_request(tmp_path)
    invalid = DisposableAdmissionRequest(request.creation, submission_id, request.text)
    with pytest.raises(ValueError, match='submission_id'):
        asyncio.run(create_and_submit_once(rpc, store, invalid))
    assert not rpc.calls
    assert not request.creation.record_path.exists()


def test_existing_ledger_identity_prevents_creation(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    request = make_request(tmp_path)
    rpc = FakeRPC()
    asyncio.run(create_and_submit_once(rpc, store, request))
    other = FakeRPC()
    with pytest.raises(ValueError, match='already exists'):
        asyncio.run(create_and_submit_once(other, store, request))
    assert not other.calls
