"""Provider-free Unix socket boundary probe for a Bubblewrap net namespace.

This deliberately implements no Hermes protocol, credentials, or provider traffic.
"""

import argparse
import json
import os
import socket
import subprocess
import tempfile
import threading
from pathlib import Path, PurePosixPath

TOKEN = b'hermes-local-probe-v1'


def serve_once(sock: socket.socket, outcome: list[str]) -> None:
    try:
        sock.settimeout(8)
        connection, _ = sock.accept()
        with connection:
            connection.settimeout(5)
            data = connection.recv(128)
            if data != TOKEN:
                outcome.append('invalid request')
                return
            connection.sendall(b'OK:' + TOKEN)
            outcome.append('accepted exact probe')
    except OSError as exc:
        outcome.append(type(exc).__name__)


def sandbox_command(directory: Path) -> list[str]:
    """Build a fixed, non-provider probe command with no ambient home mount."""
    return [
        '/usr/bin/bwrap',
        '--unshare-user',
        '--unshare-pid',
        '--unshare-net',
        '--die-with-parent',
        '--ro-bind',
        '/usr',
        '/usr',
        '--ro-bind',
        '/bin',
        '/bin',
        '--ro-bind',
        '/lib',
        '/lib',
        '--ro-bind',
        '/lib64',
        '/lib64',
        '--proc',
        '/proc',
        '--dev',
        '/dev',
        '--tmpfs',
        str(PurePosixPath('/').joinpath('tmp')),
        '--ro-bind',
        str(directory),
        '/gateway',
        '--chdir',
        '/',
        '/usr/bin/python3',
        '-c',
        (
            'import socket,sys; s=socket.socket(socket.AF_UNIX); '
            's.settimeout(4); s.connect("/gateway/probe.sock"); '
            's.sendall(b"hermes-local-probe-v1"); '
            'assert s.recv(128)==b"OK:hermes-local-probe-v1"; '
            'assert not __import__("os").path.exists("/home/hermes/.hermes"); '
            'print("PASS: isolated Unix socket exchange")'
        ),
    ]


def run_probe() -> dict[str, object]:
    if os.geteuid() == 0:
        msg = 'Run as hermes, never root'
        raise RuntimeError(msg)
    with tempfile.TemporaryDirectory(prefix='hermes-unix-probe-') as temp:
        directory = Path(temp)
        directory.chmod(0o700)
        address = directory / 'probe.sock'
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(address))
            address.chmod(0o600)
            listener.listen(1)
            outcome: list[str] = []
            thread = threading.Thread(target=serve_once, args=(listener, outcome))
            thread.start()
            try:
                result = subprocess.run(  # noqa: S603 - fixed Bubblewrap probe, no shell or agent input
                    sandbox_command(directory),
                    shell=False,
                    capture_output=True,
                    text=True,
                    timeout=12,
                    check=False,
                )
            finally:
                thread.join(timeout=9)
            return {
                'sandbox_exit_code': result.returncode,
                'sandbox_stdout': result.stdout.strip()[:300],
                'broker_observation': outcome,
                'passed': result.returncode == 0 and outcome == ['accepted exact probe'],
                'provider_invoked': False,
                'gateway_protocol_qualified': False,
            }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--qualify', action='store_true', required=True)
    parser.parse_args()
    try:
        report = run_probe()
    except (OSError, subprocess.TimeoutExpired, RuntimeError) as exc:
        report = {'passed': False, 'error': type(exc).__name__, 'provider_invoked': False}
    print(json.dumps(report, sort_keys=True, indent=2))
    return 0 if report['passed'] else 1


# Temporary exports for legacy module import compatibility.
__all__ = [
    'TOKEN',
    'Path',
    'PurePosixPath',
    'argparse',
    'json',
    'main',
    'os',
    'run_probe',
    'sandbox_command',
    'serve_once',
    'socket',
    'subprocess',
    'tempfile',
    'threading',
]
