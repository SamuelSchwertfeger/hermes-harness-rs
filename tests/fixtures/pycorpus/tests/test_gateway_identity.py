"""Offline identity contract tests; no gateway or model processes are started."""

import pytest

from hermes_harness.gateway.gateway_identity import (
    GatewayContractError,
    GatewayOutcomeUnknownError,
    OfflineAdmissionClient,
    Submission,
    parse_receipt,
)


class ScriptedRPC:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, str]]] = []
        self.failure: str | None = None
        self.response: object = {'admission_id': 'adm-1', 'status': 'queued'}

    def call(self, method: str, params: dict[str, str]) -> object:
        self.calls.append((method, params))
        if method == self.failure:
            msg = 'simulated disconnect'
            raise ConnectionError(msg)
        return self.response


def test_submission_keeps_both_identities() -> None:
    rpc = ScriptedRPC()
    client = OfflineAdmissionClient(rpc)
    submission = Submission('session-1', 'stable-submission')
    accepted, receipt = client.submit(submission, 'safe task')
    assert accepted.submission_id == 'stable-submission'
    assert accepted.admission_id == 'adm-1'
    assert receipt.status == 'queued'
    assert rpc.calls == [
        (
            'prompt.submit',
            {
                'session_id': 'session-1',
                'submission_id': 'stable-submission',
                'text': 'safe task',
            },
        )
    ]


def test_receipt_uses_admission_id_not_submission_id() -> None:
    rpc = ScriptedRPC()
    client = OfflineAdmissionClient(rpc)
    client.receipt(Submission('s', 'submission-x', 'adm-1'))
    assert rpc.calls[0] == ('prompt.receipt', {'session_id': 's', 'admission_id': 'adm-1'})


def test_cancel_is_not_a_terminal_assertion() -> None:
    rpc = ScriptedRPC()
    receipt = OfflineAdmissionClient(rpc).cancel_queued(Submission('s', 'sub', 'adm-1'))
    assert receipt.status == 'queued'
    assert rpc.calls[0][0] == 'prompt.cancel'


def test_identity_mismatch_is_rejected() -> None:
    rpc = ScriptedRPC()
    rpc.response = {'admission_id': 'different', 'status': 'queued'}
    with pytest.raises(GatewayContractError, match='changed admission identity'):
        OfflineAdmissionClient(rpc).receipt(Submission('s', 'sub', 'adm-1'))


def test_disconnect_never_resubmits() -> None:
    rpc = ScriptedRPC()
    rpc.failure = 'prompt.submit'
    with pytest.raises(GatewayOutcomeUnknownError, match='do not automatically retry'):
        OfflineAdmissionClient(rpc).submit(Submission('s', 'same-id'), 'hello')
    assert len(rpc.calls) == 1


def test_rejects_malformed_and_voice_stop_responses() -> None:
    for raw in (
        None,
        {'voice_stopped': True},
        {'admission_id': '', 'status': 'queued'},
        {'admission_id': 'a', 'status': ''},
        {'admission_id': 'a', 'status': 'queued', 'execution_generation': True},
    ):
        with pytest.raises(GatewayContractError):
            parse_receipt(raw)


def test_no_receipt_or_cancel_without_admission_identity() -> None:
    client = OfflineAdmissionClient(ScriptedRPC())
    with pytest.raises(GatewayContractError):
        client.receipt(Submission('s', 'sub'))
    with pytest.raises(GatewayContractError):
        client.cancel_queued(Submission('s', 'sub'))
