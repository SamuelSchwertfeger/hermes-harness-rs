"""No privileged subprocess calls: deterministic fake systemctl responses."""

import subprocess
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

from hermes_harness.sandbox.live_sandbox_plan import SandboxInputs, build_sandbox_plan
from hermes_harness.services.live_systemd_transport import (
    RestrictedSystemdTransport,
    SystemdTransportError,
)

if TYPE_CHECKING:
    from hermes_harness.sandbox.live_sandbox_plan import SandboxPlan


def _plan(tmp_path: Path) -> SandboxPlan:
    workspace = tmp_path / 'workspace'
    evidence = tmp_path / 'evidence'
    workspace.mkdir()
    evidence.mkdir()
    return build_sandbox_plan(
        SandboxInputs(
            role='worker',
            unit='hermes-job-qualification',
            workspace=workspace,
            evidence=evidence,
            executable=Path('/usr/bin/env').resolve(strict=True),
            argv=('--version',),
        )
    )


def test_denied_without_explicit_runner(tmp_path: Path) -> None:
    plan = _plan(tmp_path)
    transport = RestrictedSystemdTransport(plan)
    with pytest.raises(SystemdTransportError, match='Host commands disabled'):
        transport.invoke(plan.systemd_argv)


def test_exact_command_allowlist(tmp_path: Path) -> None:
    plan = _plan(tmp_path)
    calls: list[tuple[str, ...]] = []

    def runner(args: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
        calls.append(args)
        return subprocess.CompletedProcess(args, 0, stdout='')

    transport = RestrictedSystemdTransport(plan, runner)
    transport.invoke(plan.systemd_argv)
    transport.invoke(plan.stop_argv)
    assert calls == [plan.systemd_argv, plan.stop_argv]
    with pytest.raises(SystemdTransportError, match='not part of'):
        transport.invoke(('/usr/bin/systemctl', 'stop', 'ssh.service'))
    assert calls == [plan.systemd_argv, plan.stop_argv]


def test_inspection_requires_unit_match(tmp_path: Path) -> None:
    transport = RestrictedSystemdTransport(_plan(tmp_path))
    with pytest.raises(SystemdTransportError, match='Cross-unit'):
        transport.is_active('other')


@pytest.mark.parametrize(
    'case',
    [('active', '123', True), ('active', '0', False), ('inactive', '123', False)],
)
def test_active_requires_pid(tmp_path: Path, case: tuple[str, str, bool]) -> None:
    active, mainpid, expected = case

    def runner(args: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
        result = active if 'ActiveState' in args else mainpid
        return subprocess.CompletedProcess(args, 0, stdout=result)

    plan = _plan(tmp_path)
    assert RestrictedSystemdTransport(plan, runner).is_active(plan.unit) is expected


def test_nonzero_status_is_uncertain(tmp_path: Path) -> None:
    plan = _plan(tmp_path)

    def runner(args: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(args, 1, stdout='')

    with pytest.raises(SystemdTransportError, match='outcome must be reconciled'):
        RestrictedSystemdTransport(plan, runner).invoke(plan.systemd_argv)


def test_default_transport_cannot_run_host_commands(tmp_path: Path) -> None:
    plan = _plan(tmp_path)
    calls: list[str] = []

    def fail_if_called(_args: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
        calls.append('called')
        return subprocess.CompletedProcess([], 0, stdout='')

    transport = RestrictedSystemdTransport(plan)
    with pytest.raises(SystemdTransportError, match='disabled'):
        transport.is_inactive(plan.unit)
    assert calls == []
