"""Scripted gateway admission lifecycle tests: never contacts an actual gateway."""

import json
from typing import TYPE_CHECKING

import pytest

from hermes_harness.gateway.gateway_lifecycle import (
    AdmissionError,
    AdmissionState,
    GatewayLifecycle,
    LifecycleConfig,
)

if TYPE_CHECKING:
    from pathlib import Path


class ScriptedRPC:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, str]]] = []
        self.fail: set[str] = set()

    def call(self, method: str, params: dict[str, str]) -> dict[str, object]:
        self.calls.append((method, params))
        if method in self.fail:
            msg = 'simulated disconnect'
            raise ConnectionError(msg)
        return {'accepted': True, 'method': method}


def test_submit_records_private_evidence(tmp_path: Path) -> None:
    rpc = ScriptedRPC()
    client = GatewayLifecycle(rpc, LifecycleConfig(tmp_path))
    initial = client.new('session-1')
    result = client.submit(initial, 'hello')
    assert result.state == AdmissionState.RECEIPT_RECORDED
    assert rpc.calls[0][1]['submission_id'] == initial.submission_id
    evidence = tmp_path / f'{initial.submission_id}.json'
    assert json.loads(evidence.read_text())['state'] == 'receipt_recorded'
    expected_permissions = 0o600
    assert evidence.stat().st_mode & 0o777 == expected_permissions


def test_disconnect_never_resubmits(tmp_path: Path) -> None:
    rpc = ScriptedRPC()
    rpc.fail.add('prompt.submit')
    client = GatewayLifecycle(rpc, LifecycleConfig(tmp_path))
    unknown = client.submit(client.new('session-1'), 'hello')
    assert unknown.state == AdmissionState.SUBMIT_UNKNOWN
    reconciled = client.receipt(unknown)
    assert reconciled.state == AdmissionState.RECEIPT_RECORDED
    assert [method for method, _ in rpc.calls] == ['prompt.submit', 'prompt.receipt']


def test_cancel_is_request_not_termination(tmp_path: Path) -> None:
    client = GatewayLifecycle(ScriptedRPC(), LifecycleConfig(tmp_path))
    submitted = client.submit(client.new('session-1'), 'hello')
    cancelled = client.cancel(submitted, 'admission-1')
    assert cancelled.state == AdmissionState.CANCEL_REQUESTED
    assert cancelled.state != AdmissionState.TERMINAL_UNVERIFIED


def test_cancel_disconnect_is_unknown(tmp_path: Path) -> None:
    rpc = ScriptedRPC()
    rpc.fail.add('prompt.cancel')
    client = GatewayLifecycle(rpc, LifecycleConfig(tmp_path))
    submitted = client.submit(client.new('session-1'), 'hello')
    assert client.cancel(submitted, 'admission-1').state == AdmissionState.CANCEL_UNKNOWN


def test_live_transport_fails_closed(tmp_path: Path) -> None:
    with pytest.raises(AdmissionError, match='Live gateway'):
        GatewayLifecycle(ScriptedRPC(), LifecycleConfig(tmp_path, simulated_only=False))


def test_invalid_transitions_fail_closed(tmp_path: Path) -> None:
    client = GatewayLifecycle(ScriptedRPC(), LifecycleConfig(tmp_path))
    initial = client.new('session-1')
    with pytest.raises(AdmissionError):
        client.cancel(initial, 'admission-1')
    with pytest.raises(AdmissionError):
        client.submit(initial, '')
