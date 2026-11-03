"""Offline cancellation identity and uncertainty tests: no live Hermes activity."""

import pytest

from hermes_harness.gateway.gateway_identity import (
    GatewayContractError,
    GatewayOutcomeUnknownError,
    OfflineAdmissionClient,
    Submission,
)


class RecordingRPC:
    """One-shot scripted transport that records all outbound method calls."""

    def __init__(self, *, response: object = None, failure: OSError | None = None) -> None:
        self.response = response
        self.failure = failure
        self.calls: list[tuple[str, dict[str, str]]] = []

    def call(self, method: str, params: dict[str, str]) -> object:
        self.calls.append((method, params))
        if self.failure is not None:
            raise self.failure
        return self.response


def test_cancel_targets_admission_identity_only() -> None:
    rpc = RecordingRPC(response={'admission_id': 'adm-a', 'status': 'queued'})
    receipt = OfflineAdmissionClient(rpc).cancel_queued(
        Submission('session-a', 'submission-a', 'adm-a')
    )
    assert receipt.admission_id == 'adm-a'
    assert receipt.status == 'queued'
    assert rpc.calls == [('prompt.cancel', {'session_id': 'session-a', 'admission_id': 'adm-a'})]


@pytest.mark.parametrize('status', ['queued', 'running', 'complete', 'cancelled', 'unknown'])
def test_cancel_status_is_opaque_not_terminal_proof(status: str) -> None:
    rpc = RecordingRPC(response={'admission_id': 'adm-a', 'status': status})
    result = OfflineAdmissionClient(rpc).cancel_queued(Submission('s', 'sub', 'adm-a'))
    assert result.status == status
    assert result.admission_id == 'adm-a'
    assert len(rpc.calls) == 1


@pytest.mark.parametrize(
    'response',
    [
        None,
        {},
        {'admission_id': 'sub', 'status': 'queued'},
        {'admission_id': 'other-admission', 'status': 'queued'},
        {'admission_id': 'adm-a'},
        {'admission_id': 'adm-a', 'status': ''},
        {'admission_id': 'adm-a', 'status': 'queued', 'execution_generation': True},
        {'voice_stopped': True},
    ],
)
def test_cancel_bad_reply_cannot_be_accepted(response: object) -> None:
    rpc = RecordingRPC(response=response)
    with pytest.raises(GatewayContractError):
        OfflineAdmissionClient(rpc).cancel_queued(Submission('s', 'sub', 'adm-a'))
    assert len(rpc.calls) == 1
    assert rpc.calls[0][0] == 'prompt.cancel'


@pytest.mark.parametrize('failure', [ConnectionError('disconnect'), TimeoutError('timeout')])
def test_cancel_transport_failure_remains_ambiguous(failure: OSError) -> None:
    rpc = RecordingRPC(failure=failure)
    with pytest.raises(GatewayOutcomeUnknownError, match='do not automatically retry'):
        OfflineAdmissionClient(rpc).cancel_queued(Submission('s', 'sub', 'adm-a'))
    assert rpc.calls == [('prompt.cancel', {'session_id': 's', 'admission_id': 'adm-a'})]


@pytest.mark.parametrize('admission_id', [None, '', '  '])
def test_cancel_without_admission_identity_never_calls_gateway(
    admission_id: str | None,
) -> None:
    rpc = RecordingRPC(response={'admission_id': 'adm-a', 'status': 'queued'})
    with pytest.raises(GatewayContractError, match='admission_id'):
        OfflineAdmissionClient(rpc).cancel_queued(Submission('s', 'sub', admission_id))
    assert rpc.calls == []


def test_cancel_result_does_not_mutate_caller_submission() -> None:
    submission = Submission('s', 'sub', 'adm-a')
    rpc = RecordingRPC(response={'admission_id': 'adm-a', 'status': 'running'})
    OfflineAdmissionClient(rpc).cancel_queued(submission)
    assert submission == Submission('s', 'sub', 'adm-a')
    assert len(rpc.calls) == 1
