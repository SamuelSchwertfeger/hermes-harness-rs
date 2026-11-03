"""Fail-closed, one-shot disposable session creation for an attached gateway.

This module never discovers or starts a gateway and never submits a prompt.
"""

import json
import os
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

MAX_REQUEST_ID_LENGTH = 256


class SessionCreationError(RuntimeError):
    """Creation cannot be trusted or safely attempted."""


class SessionCreationUnknownError(SessionCreationError):
    """Creation may have happened; never send the request again automatically."""


class SessionRPC(Protocol):
    """An already-authenticated, explicitly attached RPC connection."""

    async def rpc(self, method: str, **params: object) -> object: ...


@dataclass(frozen=True, slots=True)
class SessionCreationRequest:
    """A unique durable creation identity and an operator-chosen ledger location."""

    request_id: str
    record_path: Path


@dataclass(frozen=True, slots=True)
class PreparedSession:
    """Confirmed attached session; only valid on the RPC used for creation."""

    session_id: str
    subscription_id: str
    request_id: str


def _persist(path: Path, record: dict[str, str], *, exclusive: bool) -> None:
    """Create the initial no-retry barrier atomically, then sync status updates."""
    if exclusive:
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        try:
            fd = os.open(path, flags, 0o600)
        except FileExistsError as exc:
            msg = 'Creation record already exists; do not retry the request'
            raise SessionCreationError(msg) from exc
    else:
        fd = os.open(path, os.O_WRONLY | os.O_TRUNC)
    with os.fdopen(fd, 'w', encoding='utf-8') as stream:
        json.dump(record, stream, sort_keys=True)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())


def _disposable_cwd() -> str:
    """Resolve and validate a safe default working directory synchronously."""
    cwd = Path(tempfile.gettempdir()).resolve(strict=True)
    if not cwd.is_dir():
        msg = 'Disposable session cwd is not a directory'
        raise SessionCreationError(msg)
    return str(cwd)


def _verify_response(response: object, request: SessionCreationRequest) -> PreparedSession:
    if not isinstance(response, dict):
        msg = 'session.create returned an invalid response'
        raise SessionCreationUnknownError(msg)
    sid = response.get('session_id')
    subscription = response.get('subscription_id')
    if not isinstance(sid, str) or not sid.startswith('local-') or not sid.strip():
        msg = 'session.create returned no valid local session identity'
        raise SessionCreationUnknownError(msg)
    if not isinstance(subscription, str) or not subscription.strip():
        msg = 'session.create did not confirm attachment to this connection'
        raise SessionCreationUnknownError(msg)
    return PreparedSession(sid, subscription, request.request_id)


async def prepare_disposable_session(
    rpc: SessionRPC,
    request: SessionCreationRequest,
) -> PreparedSession:
    """Create once, requiring same-connection attachment evidence.

    The exclusive record is a no-retry barrier, *not* a transaction log. A power
    failure during an update may corrupt it; even then an existing file always
    prevents automatic re-creation. This function does not submit a prompt.
    """
    if not isinstance(request.request_id, str) or not request.request_id.strip():
        msg = 'request_id must be nonblank'
        raise ValueError(msg)
    if len(request.request_id) > MAX_REQUEST_ID_LENGTH:
        msg = 'request_id exceeds Hermes limit'
        raise ValueError(msg)
    # Never inherit the operator profile's tools or workspace. Hermes treats an
    # explicit empty toolsets list as opt-out from all toolsets (including the
    # context-engine toolset); the temporary directory is the session cwd.
    cwd = _disposable_cwd()
    record = {'request_id': request.request_id, 'status': 'unknown'}
    _persist(request.record_path, record, exclusive=True)
    try:
        response = await rpc.rpc(
            'session.create',
            request_id=request.request_id,
            source='cli',
            cwd=cwd,
            toolsets=[],
            max_turns=1,
            run_budget=60,
        )
        prepared = _verify_response(response, request)
    except Exception as exc:
        msg = 'Session creation outcome unknown; do not retry or submit'
        raise SessionCreationUnknownError(msg) from exc
    record.update({'status': 'attached', 'session_id': prepared.session_id})
    try:
        _persist(request.record_path, record, exclusive=False)
    except OSError as exc:
        msg = 'Session created but its identity could not be durably recorded'
        raise SessionCreationUnknownError(msg) from exc
    return prepared


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_REQUEST_ID_LENGTH',
    'Path',
    'PreparedSession',
    'Protocol',
    'SessionCreationError',
    'SessionCreationRequest',
    'SessionCreationUnknownError',
    'SessionRPC',
    '_disposable_cwd',
    '_persist',
    '_verify_response',
    'dataclass',
    'json',
    'os',
    'prepare_disposable_session',
    'tempfile',
]
