"""Single-request Unix socket client for pre-authorized sandbox invocation."""

import json
import socket

from hermes_harness.broker.live_capability_broker import MAX_MESSAGE_BYTES, MAX_RESPONSE_BYTES


class CapabilityClientError(RuntimeError):
    """The broker response cannot be trusted or submission is uncertain."""


def submit_once(socket_path: str, request: dict[str, str]) -> str:
    """Send one request, without any transport retry or configurable RPC method."""
    raw = json.dumps(request, separators=(',', ':')).encode()
    if not raw or len(raw) > MAX_MESSAGE_BYTES:
        msg = 'Capability request exceeds maximum frame size'
        raise CapabilityClientError(msg)
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
        conn.settimeout(5)
        try:
            conn.connect(socket_path)
            conn.sendall(len(raw).to_bytes(4, 'big') + raw)
            header = _receive(conn, 4)
            length = int.from_bytes(header, 'big')
            if length < 1 or length > MAX_RESPONSE_BYTES:
                msg = 'Invalid broker response length; outcome uncertain'
                raise CapabilityClientError(msg)
            decoded = json.loads(_receive(conn, length))
        except (OSError, ValueError) as exc:
            msg = 'Broker communication outcome uncertain; do not retry'
            raise CapabilityClientError(msg) from exc
    return _validate_admission(decoded)


def _validate_admission(decoded: object) -> str:
    if not isinstance(decoded, dict) or set(decoded) != {'admission_id'}:
        msg = 'Invalid broker admission; outcome uncertain'
        raise CapabilityClientError(msg)
    admission = decoded['admission_id']
    if not isinstance(admission, str) or not admission.strip():
        msg = 'Blank broker admission; outcome uncertain'
        raise CapabilityClientError(msg)
    return admission


def _receive(conn: socket.socket, count: int) -> bytes:
    pieces = bytearray()
    while len(pieces) < count:
        chunk = conn.recv(count - len(pieces))
        if not chunk:
            msg = 'Incomplete broker response; outcome uncertain'
            raise CapabilityClientError(msg)
        pieces.extend(chunk)
    return bytes(pieces)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_MESSAGE_BYTES',
    'MAX_RESPONSE_BYTES',
    'CapabilityClientError',
    '_receive',
    '_validate_admission',
    'json',
    'socket',
    'submit_once',
]
