"""Bounded, single-use Unix socket capability broker; no provider adapter by default.

A broker process runs outside Bubblewrap. Only the broker can access its gateway
adapter. The sandbox sees one explicitly mounted socket and a one-shot capability.
"""

import hashlib
import hmac
import json
import os
import socket
import sqlite3
import struct
from collections.abc import Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from pathlib import Path


MAX_MESSAGE_BYTES = 16_384
MAX_RESPONSE_BYTES = 16_384
MAX_TEXT_BYTES = 8_192
TOKEN_BYTES = 32
SHA256_HEX_LENGTH = 64


class BrokerDeniedError(RuntimeError):
    """Request cannot be safely admitted or executed."""


@dataclass(frozen=True, slots=True)
class BoundRequest:
    """One exact authorized invocation; role text is not agent-controlled."""

    task_id: str
    role: str
    session_id: str
    submission_id: str
    method: str
    text_sha256: str


class CapabilityLedger:
    """Atomic one-time reservation and no-replay journal (no cleartext secrets)."""

    def __init__(self, path: Path) -> None:
        if path.is_symlink():
            msg = 'Capability database must not be a symlink'
            raise BrokerDeniedError(msg)
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.path = path
        with self._connect() as db:
            db.execute("""CREATE TABLE IF NOT EXISTS capabilities (
                token_hash TEXT PRIMARY KEY, task_id TEXT NOT NULL,
                role TEXT NOT NULL, session_id TEXT NOT NULL,
                submission_id TEXT NOT NULL UNIQUE, method TEXT NOT NULL,
                text_sha256 TEXT NOT NULL, state TEXT NOT NULL,
                result_json TEXT
            )""")

    def _connect(self) -> sqlite3.Connection:
        db = sqlite3.connect(self.path, timeout=5, isolation_level=None)
        db.execute('PRAGMA busy_timeout=5000')
        db.execute('PRAGMA synchronous=FULL')
        return db

    def issue(self, request: BoundRequest) -> str:
        """Operator-only issuance; callers must separately authorize the task."""
        if request.role not in ('worker', 'reviewer') or request.method != 'prompt.submit':
            msg = 'Only a bounded role-specific prompt submission may be issued'
            raise BrokerDeniedError(msg)
        if not all((request.task_id, request.session_id, request.submission_id)):
            msg = 'Empty identity field'
            raise BrokerDeniedError(msg)
        if len(request.text_sha256) != SHA256_HEX_LENGTH or any(
            c not in '0123456789abcdef' for c in request.text_sha256
        ):
            msg = 'Expected lowercase SHA256 digest'
            raise BrokerDeniedError(msg)
        token = os.urandom(TOKEN_BYTES).hex()
        digest = hashlib.sha256(bytes.fromhex(token)).hexdigest()
        with self._connect() as db:
            db.execute(
                'INSERT INTO capabilities VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)',
                (
                    digest,
                    request.task_id,
                    request.role,
                    request.session_id,
                    request.submission_id,
                    request.method,
                    request.text_sha256,
                    'issued',
                ),
            )
        return token

    def reserve(self, token: str, data: dict[str, object]) -> BoundRequest:
        """Durably consume before any external side effect."""
        digest = _token_digest(token)
        with self._connect() as db:
            db.execute('BEGIN IMMEDIATE')
            row = db.execute(
                'SELECT task_id, role, session_id, submission_id, method, text_sha256, state '
                'FROM capabilities WHERE token_hash = ?',
                (digest,),
            ).fetchone()
            if row is None or row[6] != 'issued':
                db.execute('ROLLBACK')
                msg = 'Capability unknown or already consumed'
                raise BrokerDeniedError(msg)
            expected = BoundRequest(*row[:6])
            _validate_bound_request(expected, data)
            db.execute(
                "UPDATE capabilities SET state = 'submitted_uncertain' WHERE token_hash = ?",
                (digest,),
            )
            db.execute('COMMIT')
            return expected

    def complete(self, token: str, response: dict[str, object]) -> None:
        digest = _token_digest(token)
        with self._connect() as db:
            db.execute(
                "UPDATE capabilities SET state = 'response_recorded', result_json = ? "
                "WHERE token_hash = ? AND state = 'submitted_uncertain'",
                (json.dumps(response, sort_keys=True), digest),
            )

    def status(self, token: str) -> str | None:
        try:
            raw = bytes.fromhex(token)
        except ValueError:
            return None
        digest = hashlib.sha256(raw).hexdigest()
        with self._connect() as db:
            row = db.execute(
                'SELECT state FROM capabilities WHERE token_hash = ?', (digest,)
            ).fetchone()
        return str(row[0]) if row is not None else None


def _token_digest(token: str) -> str:
    try:
        raw = bytes.fromhex(token)
    except ValueError as exc:
        msg = 'Malformed capability'
        raise BrokerDeniedError(msg) from exc
    if len(raw) != TOKEN_BYTES:
        msg = 'Malformed capability length'
        raise BrokerDeniedError(msg)
    return hashlib.sha256(raw).hexdigest()


def _validate_bound_request(expected: BoundRequest, data: dict[str, object]) -> None:
    fields = ('task_id', 'role', 'session_id', 'submission_id', 'method')
    if any(data.get(field) != getattr(expected, field) for field in fields):
        msg = 'Capability identity mismatch'
        raise BrokerDeniedError(msg)
    text = data.get('text')
    if not isinstance(text, str) or not text.strip() or len(text.encode()) > MAX_TEXT_BYTES:
        msg = 'Invalid or oversized prompt'
        raise BrokerDeniedError(msg)
    if not hmac.compare_digest(hashlib.sha256(text.encode()).hexdigest(), expected.text_sha256):
        msg = 'Prompt digest mismatch'
        raise BrokerDeniedError(msg)


GatewayAdapter = Callable[[str, dict[str, str]], dict[str, object]]


class CapabilityBroker:
    """Never trusts an agent-specified gateway destination or RPC operation."""

    def __init__(
        self,
        ledger: CapabilityLedger,
        adapter: GatewayAdapter | None = None,
        *,
        bound_adapter: Callable[[BoundRequest, str], dict[str, object]] | None = None,
    ) -> None:
        self.ledger = ledger
        self.adapter = adapter
        self.bound_adapter = bound_adapter

    def handle(self, payload: bytes) -> dict[str, object]:
        data = _decode_request(payload)
        token = data['capability']
        if self.adapter is None and self.bound_adapter is None:
            msg = 'No qualified gateway adapter configured'
            raise BrokerDeniedError(msg)
        authorized = self.ledger.reserve(token, dict[str, object](data))
        # Reservation precedes all adapter activity; never auto-retry failures.
        response = self._invoke(authorized, data['text'])
        if not isinstance(response, dict) or not isinstance(response.get('admission_id'), str):
            msg = 'Gateway admission response invalid; outcome uncertain'
            raise BrokerDeniedError(msg)
        safe = {'admission_id': response['admission_id']}
        self.ledger.complete(token, safe)
        return safe

    def _invoke(self, authorized: BoundRequest, text: str) -> dict[str, object]:
        if self.bound_adapter is not None:
            return self.bound_adapter(authorized, text)
        if self.adapter is not None:
            return self.adapter(
                authorized.method,
                {
                    'session_id': authorized.session_id,
                    'submission_id': authorized.submission_id,
                    'text': text,
                },
            )
        msg = 'No adapter configured'
        raise BrokerDeniedError(msg)


def _decode_request(payload: bytes) -> dict[str, str]:
    if len(payload) > MAX_MESSAGE_BYTES:
        msg = 'Request too large'
        raise BrokerDeniedError(msg)
    try:
        data = json.loads(payload)
    except (ValueError, UnicodeDecodeError) as exc:
        msg = 'Invalid JSON request'
        raise BrokerDeniedError(msg) from exc
    fields = {'capability', 'task_id', 'role', 'session_id', 'submission_id', 'method', 'text'}
    if not isinstance(data, dict) or set(data) != fields:
        msg = 'Unexpected request fields'
        raise BrokerDeniedError(msg)
    if any(not isinstance(value, str) for value in data.values()):
        msg = 'Request values must be strings'
        raise BrokerDeniedError(msg)
    return cast('dict[str, str]', data)


def serve_connection(conn: socket.socket, broker: CapabilityBroker, expected_uid: int) -> None:
    """Serve exactly one framed request from an authorized same-host UID."""
    conn.settimeout(5)
    credentials = conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize('3i'))
    _pid, uid, _gid = struct.unpack('3i', credentials)
    if uid != expected_uid:
        msg = 'Peer UID not authorized'
        raise BrokerDeniedError(msg)
    header = _receive(conn, 4)
    count = int.from_bytes(header, 'big')
    if count < 1 or count > MAX_MESSAGE_BYTES:
        msg = 'Frame exceeds limit'
        raise BrokerDeniedError(msg)
    payload = _receive(conn, count)
    result = json.dumps(broker.handle(payload), separators=(',', ':')).encode()
    if len(result) > MAX_RESPONSE_BYTES:
        msg = 'Response exceeds limit'
        raise BrokerDeniedError(msg)
    conn.sendall(len(result).to_bytes(4, 'big') + result)


def _receive(conn: socket.socket, count: int) -> bytes:
    result = bytearray()
    while len(result) < count:
        chunk = conn.recv(count - len(result))
        if not chunk:
            msg = 'Socket closed before full frame'
            raise BrokerDeniedError(msg)
        result.extend(chunk)
    return bytes(result)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_MESSAGE_BYTES',
    'MAX_RESPONSE_BYTES',
    'MAX_TEXT_BYTES',
    'SHA256_HEX_LENGTH',
    'TOKEN_BYTES',
    'BoundRequest',
    'BrokerDeniedError',
    'Callable',
    'CapabilityBroker',
    'CapabilityLedger',
    'GatewayAdapter',
    '_decode_request',
    '_receive',
    '_token_digest',
    '_validate_bound_request',
    'cast',
    'dataclass',
    'hashlib',
    'hmac',
    'json',
    'os',
    'serve_connection',
    'socket',
    'sqlite3',
    'struct',
]
