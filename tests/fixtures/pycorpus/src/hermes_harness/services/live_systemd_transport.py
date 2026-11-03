"""Restricted systemd transport for pre-reviewed sandbox plans.

Constructing this transport does not authorize execution. Only an explicitly
injected subprocess runner can perform operations, and no shell is involved.
"""

import re
import subprocess
from collections.abc import Callable
from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Sequence

    from hermes_harness.sandbox.live_sandbox_plan import SandboxPlan

_UNIT = re.compile(r'hermes-job-[A-Za-z0-9_.-]{1,53}\Z')
_RUN = '/usr/bin/systemd-run'
_CTL = '/usr/bin/systemctl'
Runner = Callable[[tuple[str, ...]], subprocess.CompletedProcess[str]]


class SystemdTransportError(RuntimeError):
    """Systemd operation failed or returned an untrusted response."""


def _deny_runner(_argv: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
    msg = 'Host commands disabled: inject a qualified operator runner explicitly'
    raise SystemdTransportError(msg)


@dataclass(frozen=True, slots=True)
class RestrictedSystemdTransport:
    """Operate exactly one approved unit and exact launch/stop arguments.

    Caller must separately qualify privileged invocation, filesystem mounts,
    gateway connectivity and process isolation before injecting a real runner.
    """

    plan: SandboxPlan
    runner: Runner = _deny_runner

    def __post_init__(self) -> None:
        if not _UNIT.fullmatch(self.plan.unit):
            msg = 'Invalid service unit'
            raise SystemdTransportError(msg)
        if self.plan.role not in ('worker', 'reviewer'):
            msg = 'Invalid role'
            raise SystemdTransportError(msg)
        if not self.plan.systemd_argv or self.plan.systemd_argv[0] != _RUN:
            msg = 'Expected systemd-run launch plan'
            raise SystemdTransportError(msg)
        if self.plan.stop_argv != (_CTL, 'stop', f'{self.plan.unit}.service'):
            msg = 'Unexpected service stop command'
            raise SystemdTransportError(msg)

    def _run(self, argv: tuple[str, ...]) -> str:
        completed = self.runner(argv)
        if completed.returncode != 0:
            msg = 'Systemd command failed; outcome must be reconciled without replay'
            raise SystemdTransportError(msg)
        return completed.stdout

    def invoke(self, argv: Sequence[str]) -> None:
        """Only invoke the exact audited launch or stop vector."""
        command = tuple(argv)
        if command not in (self.plan.systemd_argv, self.plan.stop_argv):
            msg = 'Command is not part of the authorized sandbox plan'
            raise SystemdTransportError(msg)
        self._run(command)

    def _property(self, unit: str, name: str) -> str:
        if unit != self.plan.unit:
            msg = 'Cross-unit inspection denied'
            raise SystemdTransportError(msg)
        output = self._run((_CTL, 'show', '--property', name, '--value', f'{unit}.service'))
        return output.strip()

    def is_active(self, unit: str) -> bool:
        """Require both active state and a live main process."""
        if self._property(unit, 'ActiveState') != 'active':
            return False
        pid = self._property(unit, 'MainPID')
        return pid.isascii() and pid.isdigit() and int(pid) > 0

    def is_inactive(self, unit: str) -> bool:
        """Require stopped service and no main process."""
        return (
            self._property(unit, 'ActiveState') == 'inactive'
            and self._property(unit, 'MainPID') == '0'
        )


def operator_subprocess_runner(argv: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
    """Opt-in host runner; caller must explicitly inject this after qualification.

    Never elevate privileges, spawn a shell, or assume timeout means failure.
    """
    try:
        return subprocess.run(  # noqa: S603
            argv,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        msg = 'Systemd request timed out; outcome unknown, do not resubmit'
        raise SystemdTransportError(msg) from exc


# Temporary exports for legacy module import compatibility.
__all__ = [
    '_CTL',
    '_RUN',
    '_UNIT',
    'Callable',
    'RestrictedSystemdTransport',
    'Runner',
    'SystemdTransportError',
    '_deny_runner',
    'dataclass',
    'operator_subprocess_runner',
    're',
    'subprocess',
]
