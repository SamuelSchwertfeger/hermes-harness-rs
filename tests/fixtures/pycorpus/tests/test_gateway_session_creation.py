"""No-retry and subscription-evidence checks for session creation."""

import asyncio
import json
import tempfile
from pathlib import Path

import pytest

from hermes_harness.gateway.gateway_session_creation import (
    SessionCreationError,
    SessionCreationRequest,
    SessionCreationUnknownError,
    prepare_disposable_session,
)

SESSION_ID = 'local-aaaaaaaaaaaaaaaa'
SUBSCRIPTION_ID = 'subscription-1'


class StubRPC:
    """Count session-create requests and optionally raise before returning."""

    def __init__(self, result: object) -> None:
        self.result = result
        self.calls: list[tuple[str, dict[str, object]]] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        if isinstance(self.result, Exception):
            raise self.result
        return self.result


def _request(tmp_path: Path) -> SessionCreationRequest:
    return SessionCreationRequest(
        request_id='m3-disposable-test', record_path=tmp_path / 'creation.json'
    )


def _success() -> dict[str, str]:
    return {'session_id': SESSION_ID, 'subscription_id': SUBSCRIPTION_ID}


def test_creation_records_attached_session(tmp_path: Path) -> None:
    rpc = StubRPC(_success())
    request = _request(tmp_path)
    result = asyncio.run(prepare_disposable_session(rpc, request))
    assert result.session_id == SESSION_ID
    assert result.subscription_id == SUBSCRIPTION_ID
    method, params = rpc.calls[0]
    assert method == 'session.create'
    assert params == {
        'request_id': request.request_id,
        'source': 'cli',
        'cwd': str(Path(tempfile.gettempdir()).resolve()),
        'toolsets': [],
        'max_turns': 1,
        'run_budget': 60,
    }
    assert len(rpc.calls) == 1
    assert json.loads(request.record_path.read_text()) == {
        'request_id': request.request_id,
        'session_id': SESSION_ID,
        'status': 'attached',
    }


@pytest.mark.parametrize(
    'response', [None, {}, {'session_id': SESSION_ID}, {'subscription_id': 'sub'}]
)
def test_missing_attachment_evidence_fails_closed(tmp_path: Path, response: object) -> None:
    rpc = StubRPC(response)
    request = _request(tmp_path)
    with pytest.raises(SessionCreationUnknownError, match='outcome unknown'):
        asyncio.run(prepare_disposable_session(rpc, request))
    assert json.loads(request.record_path.read_text())['status'] == 'unknown'
    assert len(rpc.calls) == 1


def test_transport_failure_never_retries(tmp_path: Path) -> None:
    rpc = StubRPC(ConnectionError('disconnected'))
    request = _request(tmp_path)
    with pytest.raises(SessionCreationUnknownError, match='outcome unknown'):
        asyncio.run(prepare_disposable_session(rpc, request))
    assert len(rpc.calls) == 1
    with pytest.raises(SessionCreationError, match='already exists'):
        asyncio.run(prepare_disposable_session(rpc, request))
    assert len(rpc.calls) == 1


def test_successful_creation_never_repeated(tmp_path: Path) -> None:
    rpc = StubRPC(_success())
    request = _request(tmp_path)
    asyncio.run(prepare_disposable_session(rpc, request))
    with pytest.raises(SessionCreationError, match='already exists'):
        asyncio.run(prepare_disposable_session(rpc, request))
    assert len(rpc.calls) == 1


@pytest.mark.parametrize('request_id', ['', ' ', 'x' * 257])
def test_invalid_identity_does_not_contact_gateway(tmp_path: Path, request_id: str) -> None:
    rpc = StubRPC(_success())
    request = SessionCreationRequest(request_id, tmp_path / 'creation.json')
    with pytest.raises(ValueError, match='request_id'):
        asyncio.run(prepare_disposable_session(rpc, request))
    assert not rpc.calls
    assert not request.record_path.exists()


def test_existing_creation_barrier_is_not_overwritten(tmp_path: Path) -> None:
    request = _request(tmp_path)
    request.record_path.write_text('not even valid json')
    rpc = StubRPC(_success())
    with pytest.raises(SessionCreationError, match='already exists'):
        asyncio.run(prepare_disposable_session(rpc, request))
    assert request.record_path.read_text() == 'not even valid json'
    assert not rpc.calls
