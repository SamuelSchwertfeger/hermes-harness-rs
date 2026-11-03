"""Offline safety tests for the explicitly gated live-admission probe."""

import asyncio
from dataclasses import dataclass
from typing import TYPE_CHECKING

import pytest

from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_identity import (
    GatewayContractError,
    GatewayOutcomeUnknownError,
    Submission,
)
from hermes_harness.gateway.gateway_live_admission import (
    LiveAdmissionRequest,
    submit_once,
)

if TYPE_CHECKING:
    from collections.abc import Callable
    from pathlib import Path


@dataclass(frozen=True, slots=True)
class RPCResults:
    """Responses or injected exceptions for the scripted RPC fake."""

    submit_result: object | Exception | None = None
    receipt_result: object | Exception | None = None


class FakeRPC:
    """Scripted async RPC that records every call and can inject a fault."""

    def __init__(
        self,
        store: AdmissionStore,
        submission_id: str,
        *,
        results: RPCResults | None = None,
    ) -> None:
        self.store = store
        self.submission_id = submission_id
        self.results = results or RPCResults()
        self.calls: list[tuple[str, dict[str, object]]] = []
        self.before_submit: Callable[[], None] | None = None

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        if method == 'prompt.submit':
            record = self.store.get(self.submission_id)
            assert record is not None
            assert record.local_state is LocalState.OUTCOME_UNKNOWN
            if self.before_submit is not None:
                self.before_submit()
            result = self.results.submit_result
            if isinstance(result, Exception):
                raise result
            return {'admission_id': 'adm-1', 'status': 'queued'} if result is None else result
        if method == 'prompt.receipt':
            record = self.store.get(self.submission_id)
            assert record is not None
            assert record.admission_id == 'adm-1'
            result = self.results.receipt_result
            if isinstance(result, Exception):
                raise result
            return {'admission_id': 'adm-1', 'status': 'queued'} if result is None else result
        msg = f'Unexpected RPC method: {method}'
        raise AssertionError(msg)


def make_store(tmp_path: Path) -> AdmissionStore:
    return AdmissionStore(tmp_path / 'admissions.sqlite')


def make_submission() -> Submission:
    return Submission(session_id='isolated-test-session', submission_id='probe-001')


def test_submit_once_persists_identity_before_receipt_lookup(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    submission = make_submission()
    rpc = FakeRPC(store, submission.submission_id)

    result = asyncio.run(
        submit_once(
            store,
            LiveAdmissionRequest(submission, 'Reply with M3_OK only; do not use tools.'),
            rpc=rpc,
        )
    )

    assert [method for method, _ in rpc.calls] == ['prompt.submit', 'prompt.receipt']
    assert result.admission_id == 'adm-1'
    assert result.submit_status == 'queued'
    record = store.get(submission.submission_id)
    assert record is not None
    assert record.admission_id == 'adm-1'
    assert record.local_state is LocalState.ADMITTED


def test_submit_params_are_minimal_and_stable(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    submission = make_submission()
    rpc = FakeRPC(store, submission.submission_id)

    asyncio.run(submit_once(store, LiveAdmissionRequest(submission, 'M3 probe'), rpc=rpc))

    method, params = rpc.calls[0]
    assert method == 'prompt.submit'
    assert params == {
        'session_id': submission.session_id,
        'submission_id': submission.submission_id,
        'text': 'M3 probe',
    }
    assert rpc.calls[1][1] == {
        'session_id': submission.session_id,
        'admission_id': 'adm-1',
    }


def test_submit_transport_failure_stays_unknown_and_never_retries(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    submission = make_submission()
    rpc = FakeRPC(
        store, submission.submission_id, results=RPCResults(submit_result=OSError('disconnected'))
    )

    with pytest.raises(GatewayOutcomeUnknownError):
        asyncio.run(submit_once(store, LiveAdmissionRequest(submission, 'M3 probe'), rpc=rpc))

    record = store.get(submission.submission_id)
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert record.admission_id is None
    assert [method for method, _ in rpc.calls] == ['prompt.submit']


def test_malformed_submit_response_stays_unknown_and_never_retries(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    submission = make_submission()
    rpc = FakeRPC(
        store, submission.submission_id, results=RPCResults(submit_result={'status': 'queued'})
    )

    with pytest.raises(GatewayOutcomeUnknownError):
        asyncio.run(submit_once(store, LiveAdmissionRequest(submission, 'M3 probe'), rpc=rpc))

    record = store.get(submission.submission_id)
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert [method for method, _ in rpc.calls] == ['prompt.submit']


def test_receipt_read_failure_preserves_known_admission_identity(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    submission = make_submission()
    rpc = FakeRPC(
        store,
        submission.submission_id,
        results=RPCResults(receipt_result=OSError('receipt disconnected')),
    )

    with pytest.raises(OSError, match='receipt disconnected'):
        asyncio.run(submit_once(store, LiveAdmissionRequest(submission, 'M3 probe'), rpc=rpc))

    record = store.get(submission.submission_id)
    assert record is not None
    assert record.admission_id == 'adm-1'
    assert record.local_state is LocalState.ADMITTED
    assert [method for method, _ in rpc.calls] == ['prompt.submit', 'prompt.receipt']


def test_mismatched_receipt_cannot_change_known_admission_id(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    submission = make_submission()
    rpc = FakeRPC(
        store,
        submission.submission_id,
        results=RPCResults(receipt_result={'admission_id': 'adm-other', 'status': 'queued'}),
    )

    with pytest.raises(GatewayContractError, match='changed admission identity'):
        asyncio.run(submit_once(store, LiveAdmissionRequest(submission, 'M3 probe'), rpc=rpc))

    record = store.get(submission.submission_id)
    assert record is not None
    assert record.admission_id == 'adm-1'
    assert record.local_state is LocalState.ADMITTED
    assert [method for method, _ in rpc.calls] == ['prompt.submit', 'prompt.receipt']


def test_duplicate_submission_id_is_refused_before_rpc(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    submission = make_submission()
    store.prepare(submission)
    rpc = FakeRPC(store, submission.submission_id)

    with pytest.raises(RuntimeError, match='already exists'):
        asyncio.run(submit_once(store, LiveAdmissionRequest(submission, 'M3 probe'), rpc=rpc))

    assert rpc.calls == []


@pytest.mark.parametrize('text', ['', '   '])
def test_blank_probe_text_refused_before_ledger_or_rpc(tmp_path: Path, text: str) -> None:
    store = make_store(tmp_path)
    submission = make_submission()
    rpc = FakeRPC(store, submission.submission_id)

    with pytest.raises(ValueError, match='nonblank'):
        asyncio.run(submit_once(store, LiveAdmissionRequest(submission, text), rpc=rpc))

    assert store.get(submission.submission_id) is None
    assert rpc.calls == []


def test_submission_with_existing_admission_id_is_refused(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    submission = Submission('isolated-test-session', 'probe-001', 'adm-existing')
    rpc = FakeRPC(store, submission.submission_id)

    with pytest.raises(ValueError, match='without an admission ID'):
        asyncio.run(submit_once(store, LiveAdmissionRequest(submission, 'M3 probe'), rpc=rpc))

    assert rpc.calls == []
