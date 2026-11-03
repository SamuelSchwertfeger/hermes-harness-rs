"""Provider-free broker admission authorization and socket lifecycle tests."""

import hashlib
import os
import sqlite3
import threading
from typing import TYPE_CHECKING

import pytest

from hermes_harness.broker.live_broker_admission import (
    OperatorAuthorizationError,
    OperatorGrant,
    OperatorGrantStore,
)
from hermes_harness.broker.live_broker_listener import (
    BrokerListenerError,
    ListenerSettings,
    serve_until,
)
from hermes_harness.broker.live_capability_broker import (
    BoundRequest,
    CapabilityBroker,
    CapabilityLedger,
)
from hermes_harness.broker.live_capability_client import CapabilityClientError, submit_once

if TYPE_CHECKING:
    from pathlib import Path


def test_authorization_is_exactly_once_and_restart_safe(tmp_path: Path) -> None:
    path = tmp_path / 'grants.db'
    store = OperatorGrantStore(path)
    grant = OperatorGrant('task', 'worker', 'session', 'sub', 'hermes-job-1', 'a' * 64)
    with pytest.raises(OperatorAuthorizationError, match='confirmation'):
        store.authorize(grant)
    store.authorize(grant, human_confirmed=True)
    store.consume(grant)
    with pytest.raises(OperatorAuthorizationError, match='unconsumed'):
        OperatorGrantStore(path).consume(grant)
    with pytest.raises(sqlite3.IntegrityError, match='UNIQUE'):
        store.authorize(grant, human_confirmed=True)


def test_grant_mismatch_never_consumes_valid_grant(tmp_path: Path) -> None:
    store = OperatorGrantStore(tmp_path / 'grants.db')
    grant = OperatorGrant('task', 'reviewer', 'session', 'sub', 'hermes-job-2', 'b' * 64)
    store.authorize(grant, human_confirmed=True)
    with pytest.raises(OperatorAuthorizationError):
        store.consume(OperatorGrant('task', 'worker', 'session', 'sub', 'hermes-job-2', 'b' * 64))
    store.consume(grant)


def _request_data(token: str, request: BoundRequest, text: str) -> dict[str, str]:
    return {
        'capability': token,
        'task_id': request.task_id,
        'role': request.role,
        'session_id': request.session_id,
        'submission_id': request.submission_id,
        'method': request.method,
        'text': text,
    }


def test_listener_framing_and_replay_after_restart(tmp_path: Path) -> None:
    text = 'bounded task'
    request = BoundRequest(
        'task',
        'worker',
        'session',
        'sub',
        'prompt.submit',
        hashlib.sha256(text.encode()).hexdigest(),
    )
    ledger = CapabilityLedger(tmp_path / 'capability.db')
    token = ledger.issue(request)
    calls: list[str] = []

    def adapter(bound: BoundRequest, prompt: str) -> dict[str, object]:
        assert bound == request
        calls.append(prompt)
        return {'admission_id': 'receipt'}

    path = tmp_path / 'broker.sock'
    stop = threading.Event()
    ready = threading.Event()
    thread = threading.Thread(
        target=serve_until,
        args=(
            ListenerSettings(path, os.geteuid(), max_connections=2, ready=ready),
            CapabilityBroker(ledger, bound_adapter=adapter),
            stop,
        ),
    )
    thread.start()
    try:
        assert ready.wait(timeout=5), 'Broker listener did not start accepting connections'
        assert submit_once(str(path), _request_data(token, request, text)) == 'receipt'
        with pytest.raises(CapabilityClientError, match='uncertain'):
            submit_once(str(path), _request_data(token, request, text))
    finally:
        stop.set()
        thread.join(timeout=5)
    assert not path.exists()
    assert calls == [text]
    assert CapabilityLedger(ledger.path).status(token) == 'response_recorded'


def test_listener_refuses_existing_paths(tmp_path: Path) -> None:
    path = tmp_path / 'broker.sock'
    path.write_text('not a socket')
    with pytest.raises(BrokerListenerError, match='existing'):
        serve_until(
            ListenerSettings(path, os.geteuid()),
            CapabilityBroker(CapabilityLedger(tmp_path / 'db')),
            threading.Event(),
        )
