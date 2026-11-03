import json
import os
import re
import subprocess
from collections.abc import Callable
from dataclasses import asdict, dataclass
from typing import TYPE_CHECKING

"""Opt-in, provider-free host verification for existing systemd/Bubblewrap controls.

This module does not authorize live gateway traffic. It only executes a fixed
/bin/sleep service inside the operator-selected qualified sandbox profile.
"""


if TYPE_CHECKING:
    from collections.abc import Sequence
    from pathlib import Path

_UNIT = re.compile(r'hermes-verify-[a-z0-9]{8,24}\Z')
_PROPERTIES = (
    'ActiveState',
    'MainPID',
    'User',
    'MemoryMax',
    'CPUQuotaPerSecUSec',
    'TasksMax',
    'KillMode',
    'ControlGroup',
)
_COMMAND_TIMEOUT = 15


class HostProbeError(RuntimeError):
    """Verification failed; never infer live runtime qualification."""


@dataclass(frozen=True, slots=True)
class ProbeObservation:
    """One non-authoritative host observation, safe to save as JSON."""

    unit: str
    values: dict[str, str]
    cgroup_pids_before_stop: tuple[int, ...]
    pids_remaining_after_stop: tuple[int, ...]
    final_state: str
    qualified: bool = False

    def as_json(self) -> str:
        """Print allowlisted evidence only, not process arguments or secrets."""
        return json.dumps(asdict(self), sort_keys=True, indent=2)


Runner = Callable[[tuple[str, ...]], subprocess.CompletedProcess[str]]


def _disabled(_argv: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
    msg = 'Host execution disabled; explicitly provide a qualified operator runner'
    raise HostProbeError(msg)


def operator_runner(argv: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
    """Explicit opt-in, non-shell host runner; no sudo or provider invocation."""
    try:
        return subprocess.run(  # noqa: S603
            argv, check=False, text=True, capture_output=True, timeout=_COMMAND_TIMEOUT
        )
    except subprocess.TimeoutExpired as exc:
        msg = 'Host observation timed out; outcome uncertain'
        raise HostProbeError(msg) from exc


@dataclass(frozen=True, slots=True)
class ProbeCommands:
    """Fixed, operator-inspectable host commands with no arbitrary argv."""

    unit: str

    def __post_init__(self) -> None:
        if not _UNIT.fullmatch(self.unit):
            msg = 'Unit identity is outside the disposable verification namespace'
            raise HostProbeError(msg)

    def show(self, name: str) -> tuple[str, ...]:
        """Read a single allowlisted unit property."""
        if name not in _PROPERTIES:
            msg = 'Unrecognized systemd property'
            raise HostProbeError(msg)
        return ('/usr/bin/systemctl', 'show', '--property', name, '--value', f'{self.unit}.service')

    def stop(self) -> tuple[str, ...]:
        """Stop only the exact verified unit."""
        return ('/usr/bin/systemctl', 'stop', f'{self.unit}.service')


def _invoke(runner: Runner, command: tuple[str, ...]) -> str:
    result = runner(command)
    if result.returncode != 0:
        msg = 'Host command failed; stop or launch outcome is not assumed'
        raise HostProbeError(msg)
    return result.stdout.strip()


def observe_unit(commands: ProbeCommands, runner: Runner = _disabled) -> dict[str, str]:
    """Read effective systemd state without modifying the service."""
    return {key: _invoke(runner, commands.show(key)) for key in _PROPERTIES}


def assess_observation(values: dict[str, str]) -> tuple[str, ...]:
    """Validate exact limits and identity; never grant gateway authorization."""
    expected = {
        'ActiveState': 'active',
        'User': 'hermes',
        'MemoryMax': '536870912',
        'CPUQuotaPerSecUSec': '500ms',
        'TasksMax': '32',
        'KillMode': 'control-group',
    }
    failures = [name for name, value in expected.items() if values.get(name) != value]
    pid = values.get('MainPID', '')
    if not pid.isascii() or not pid.isdecimal() or int(pid) == 0:
        failures.append('MainPID')
    group = values.get('ControlGroup', '')
    if not group.startswith('/system.slice/hermes-verify-') or '..' in group:
        failures.append('ControlGroup')
    return tuple(failures)


def validate_pid_snapshot(pids: Sequence[int], remaining: Sequence[int]) -> None:
    """Reject a snapshot with no actual processes or with surviving descendants."""
    if not pids or any(pid <= 1 for pid in pids) or len(set(pids)) != len(pids):
        msg = 'Invalid or empty host process snapshot'
        raise HostProbeError(msg)
    if remaining:
        msg = 'Processes remained after service termination'
        raise HostProbeError(msg)


def write_probe_evidence(path: Path, observation: ProbeObservation) -> None:
    """Create evidence exclusively; never overwrite or follow symlink targets."""
    if path.is_symlink() or path.exists() or path.parent.is_symlink():
        msg = 'Evidence path already exists or uses a symlink'
        raise HostProbeError(msg)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, 'O_NOFOLLOW'):
        flags |= os.O_NOFOLLOW
    with os.fdopen(os.open(path, flags, 0o600), 'w', encoding='utf-8') as stream:
        stream.write(observation.as_json() + '\n')
        stream.flush()
        os.fsync(stream.fileno())


# Temporary exports for legacy module import compatibility.
__all__ = [
    '_COMMAND_TIMEOUT',
    '_PROPERTIES',
    '_UNIT',
    'Callable',
    'HostProbeError',
    'ProbeCommands',
    'ProbeObservation',
    'Runner',
    '_disabled',
    '_invoke',
    'asdict',
    'assess_observation',
    'dataclass',
    'json',
    'observe_unit',
    'operator_runner',
    'os',
    're',
    'subprocess',
    'validate_pid_snapshot',
    'write_probe_evidence',
]
