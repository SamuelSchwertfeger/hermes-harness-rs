"""Bounded, private, externally managed AF_UNIX broker socket lifecycle."""

import logging
import socket
import stat
from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.broker.live_capability_broker import BrokerDeniedError, serve_connection

if TYPE_CHECKING:
    from pathlib import Path
    from threading import Event

    from hermes_harness.broker.live_capability_broker import CapabilityBroker

LOGGER = logging.getLogger(__name__)
MAX_BACKLOG = 128
ACCEPT_POLL_SECONDS = 0.2
SOCKET_MODE = 0o600


class BrokerListenerError(RuntimeError):
    """Unsafe broker socket lifecycle."""


@dataclass(frozen=True, slots=True)
class ListenerSettings:
    path: Path
    expected_uid: int
    backlog: int = 8
    max_connections: int = 32
    ready: Event | None = None


def _validate_path(settings: ListenerSettings) -> None:
    path = settings.path
    if settings.max_connections < 1 or not 1 <= settings.backlog <= MAX_BACKLOG:
        msg = 'Invalid listener limits'
        raise BrokerListenerError(msg)
    if path.is_symlink() or path.exists():
        msg = 'Refusing existing socket path'
        raise BrokerListenerError(msg)
    parent = path.parent
    if parent.is_symlink() or not parent.is_dir():
        msg = 'Socket directory must exist and be real'
        raise BrokerListenerError(msg)
    if stat.S_IMODE(parent.stat().st_mode) & 0o002:
        msg = 'World-writable socket directory rejected'
        raise BrokerListenerError(msg)


def _handle_peer(conn: socket.socket, broker: CapabilityBroker, uid: int) -> None:
    with conn:
        try:
            serve_connection(conn, broker, uid)
        except (BrokerDeniedError, OSError, ValueError) as exc:
            # Do not log exception messages: they may include sensitive material.
            LOGGER.warning('Broker peer request rejected (%s)', type(exc).__name__)


@dataclass(frozen=True, slots=True)
class _PeerLoop:
    settings: ListenerSettings
    stop: Event


def _accept_peers(
    listener: socket.socket,
    broker: CapabilityBroker,
    loop: _PeerLoop,
) -> int:
    handled = 0
    while not loop.stop.is_set() and handled < loop.settings.max_connections:
        try:
            conn, _ = listener.accept()
        except TimeoutError:
            LOGGER.debug('Broker accept polling timeout')
            continue
        handled += 1
        _handle_peer(conn, broker, loop.settings.expected_uid)
    return handled


def serve_until(settings: ListenerSettings, broker: CapabilityBroker, stop: Event) -> int:
    """Serve bounded peers; preserve existing paths and unlink owned socket."""
    _validate_path(settings)
    path = settings.path
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
        listener.bind(str(path))
        try:
            path.chmod(SOCKET_MODE)
            listener.listen(settings.backlog)
            if settings.ready is not None:
                settings.ready.set()
            listener.settimeout(ACCEPT_POLL_SECONDS)
            return _accept_peers(listener, broker, _PeerLoop(settings, stop))
        finally:
            path.unlink(missing_ok=True)


__all__ = [
    'ACCEPT_POLL_SECONDS',
    'LOGGER',
    'MAX_BACKLOG',
    'SOCKET_MODE',
    'BrokerDeniedError',
    'BrokerListenerError',
    'ListenerSettings',
    '_PeerLoop',
    '_accept_peers',
    '_handle_peer',
    '_validate_path',
    'dataclass',
    'logging',
    'serve_connection',
    'serve_until',
    'socket',
    'stat',
]
