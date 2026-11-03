"""Capability broker tests: real sqlite, bounded Unix sockets, no provider calls."""

import hashlib
import json
import os
import socket
import sqlite3
import threading
from typing import TYPE_CHECKING

import pytest

from hermes_harness.broker.live_capability_broker import (
    BoundRequest,
    BrokerDeniedError,
    CapabilityBroker,
    CapabilityLedger,
    serve_connection,
)
from hermes_harness.broker.live_capability_client import submit_once

if TYPE_CHECKING:
    from pathlib import Path


def fixture(tmp_path: Path) -> tuple[CapabilityLedger, BoundRequest, str, dict[str, str]]:
    ledger = CapabilityLedger(tmp_path / 'ledger.sqlite')
    prompt = 'Implement the bounded task'
    request = BoundRequest(
        'task-1',
        'worker',
        'session-1',
        'sub-1',
        'prompt.submit',
        hashlib.sha256(prompt.encode()).hexdigest(),
    )
    token = ledger.issue(request)
    data = {
        'capability': token,
        'task_id': request.task_id,
        'role': request.role,
        'session_id': request.session_id,
        'submission_id': request.submission_id,
        'method': request.method,
        'text': prompt,
    }
    return ledger, request, token, data


def test_one_shot_success_and_durable_no_replay(tmp_path: Path) -> None:
    ledger, _, token, data = fixture(tmp_path)
    calls: list[object] = []

    def adapter(method: str, params: dict[str, str]) -> dict[str, object]:
        calls.append((method, params))
        return {'admission_id': 'admission-1'}

    assert CapabilityBroker(ledger, adapter).handle(json.dumps(data).encode()) == {
        'admission_id': 'admission-1'
    }
    assert CapabilityLedger(ledger.path).status(token) == 'response_recorded'
    with pytest.raises(BrokerDeniedError, match='already consumed'):
        CapabilityBroker(CapabilityLedger(ledger.path), adapter).handle(json.dumps(data).encode())
    assert len(calls) == 1


def test_uncertain_failure_never_replays(tmp_path: Path) -> None:
    ledger, _, token, data = fixture(tmp_path)

    def failed(_method: str, _params: dict[str, str]) -> dict[str, object]:
        msg = 'transport outcome unknown'
        raise OSError(msg)

    with pytest.raises(OSError, match='outcome unknown'):
        CapabilityBroker(ledger, failed).handle(json.dumps(data).encode())
    assert CapabilityLedger(ledger.path).status(token) == 'submitted_uncertain'
    with pytest.raises(BrokerDeniedError, match='already consumed'):
        CapabilityBroker(ledger, failed).handle(json.dumps(data).encode())


@pytest.mark.parametrize(
    'changed', ['role', 'session_id', 'submission_id', 'text', 'method', 'task_id']
)
def test_binding_refuses_tamper(tmp_path: Path, changed: str) -> None:
    ledger, _, token, data = fixture(tmp_path)
    data[changed] = 'tampered'
    calls: list[int] = []

    def adapter(_method: str, _params: dict[str, str]) -> dict[str, object]:
        calls.append(1)
        return {'admission_id': 'a'}

    with pytest.raises(BrokerDeniedError, match='mismatch'):
        CapabilityBroker(ledger, adapter).handle(json.dumps(data).encode())
    assert ledger.status(token) == 'issued'
    assert calls == []


def test_unknown_and_duplicate_issue(tmp_path: Path) -> None:
    ledger, request, _, data = fixture(tmp_path)
    with pytest.raises(BrokerDeniedError, match='unknown'):
        CapabilityBroker(ledger, lambda *_: {}).handle(
            json.dumps({**data, 'capability': '00' * 32}).encode()
        )
    with pytest.raises(sqlite3.IntegrityError, match='UNIQUE'):
        ledger.issue(request)


def test_no_adapter_does_not_consume(tmp_path: Path) -> None:
    ledger, _, token, data = fixture(tmp_path)
    with pytest.raises(BrokerDeniedError, match='No qualified'):
        CapabilityBroker(ledger).handle(json.dumps(data).encode())
    assert ledger.status(token) == 'issued'


def test_unix_socket_framing(tmp_path: Path) -> None:  # noqa: PLR0914 - socket integration test requires independent state
    ledger, _, token, data = fixture(tmp_path)
    left, right = socket.socketpair()
    broker = CapabilityBroker(ledger, lambda _m, _p: {'admission_id': 'safe'})
    failures: list[Exception] = []

    def worker() -> None:
        try:
            serve_connection(left, broker, os.geteuid())
        except (OSError, BrokerDeniedError, ValueError) as exc:
            failures.append(exc)
        finally:
            left.close()

    thread = threading.Thread(target=worker)
    thread.start()
    frame = json.dumps(data).encode()
    right.sendall(len(frame).to_bytes(4, 'big') + frame)
    length = int.from_bytes(right.recv(4), 'big')
    assert json.loads(right.recv(length)) == {'admission_id': 'safe'}
    right.close()
    thread.join(timeout=3)
    assert failures == []
    assert ledger.status(token) == 'response_recorded'


def test_reject_oversized_and_unexpected_fields(tmp_path: Path) -> None:
    ledger, _, _, data = fixture(tmp_path)
    broker = CapabilityBroker(ledger, lambda *_: {'admission_id': 'x'})
    with pytest.raises(BrokerDeniedError, match='too large'):
        broker.handle(b'x' * 20000)
    with pytest.raises(BrokerDeniedError, match='Unexpected'):
        broker.handle(json.dumps({**data, 'arbitrary': 'unsafe'}).encode())


def test_client_over_real_unix_listener(tmp_path: Path) -> None:
    ledger, _, token, data = fixture(tmp_path)
    address = tmp_path / 'broker.sock'
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(address))
    listener.listen(1)
    failures: list[Exception] = []

    def server() -> None:
        try:
            connection, _ = listener.accept()
            with connection:
                serve_connection(
                    connection,
                    CapabilityBroker(ledger, lambda _m, _p: {'admission_id': 'verified'}),
                    os.geteuid(),
                )
        except (OSError, BrokerDeniedError, ValueError) as exc:
            failures.append(exc)
        finally:
            listener.close()

    thread = threading.Thread(target=server)
    thread.start()
    assert submit_once(str(address), data) == 'verified'
    thread.join(timeout=3)
    assert failures == []
    assert ledger.status(token) == 'response_recorded'
