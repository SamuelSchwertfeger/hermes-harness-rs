"""Durable lifecycle tests with an injected transport; no privileged subprocesses."""

from dataclasses import dataclass, field
from pathlib import Path

import pytest

from hermes_harness.sandbox.live_sandbox_plan import SandboxInputs, build_sandbox_plan
from hermes_harness.services.live_service_controller import ServiceControlError, ServiceController
from hermes_harness.services.live_service_journal import (
    ServiceJournal,
    ServiceJournalError,
    ServiceState,
)


@dataclass
class ScriptedTransport:
    """A transport that records calls without reaching systemd."""

    calls: list[tuple[str, ...]] = field(default_factory=list)
    fail: bool = False
    inactive: bool = True
    active: bool = True

    def invoke(self, argv: tuple[str, ...]) -> None:
        self.calls.append(tuple(argv))
        if self.fail:
            msg = 'simulated uncertain transport outcome'
            raise OSError(msg)

    def is_active(self, _unit: str) -> bool:
        return self.active

    def is_inactive(self, _unit: str) -> bool:
        return self.inactive


@pytest.fixture
def setup(tmp_path: Path) -> tuple[ServiceController, ScriptedTransport, SandboxInputs]:
    workspace = tmp_path / 'worktree'
    evidence = tmp_path / 'evidence'
    workspace.mkdir()
    evidence.mkdir()
    args = SandboxInputs(
        role='worker',
        unit='hermes-job-lifecycle',
        workspace=workspace,
        evidence=evidence,
        executable=Path('/usr/bin/env').resolve(strict=True),
        argv=('probe',),
    )
    transport = ScriptedTransport()
    controller = ServiceController(ServiceJournal(tmp_path / 'journal.sqlite'), transport)
    return controller, transport, args


def test_launch_and_stop_are_durable(
    setup: tuple[ServiceController, ScriptedTransport, SandboxInputs],
) -> None:
    controller, transport, inputs = setup
    plan = build_sandbox_plan(inputs)
    controller.launch_once(plan)
    assert controller.inspect(plan.unit).state == ServiceState.RUNNING_OBSERVED  # type: ignore[union-attr]
    controller.stop_once(plan)
    assert controller.inspect(plan.unit).state == ServiceState.STOPPED_OBSERVED  # type: ignore[union-attr]
    assert transport.calls == [plan.systemd_argv, plan.stop_argv]


def test_duplicate_never_replays(
    setup: tuple[ServiceController, ScriptedTransport, SandboxInputs],
) -> None:
    controller, transport, inputs = setup
    plan = build_sandbox_plan(inputs)
    controller.launch_once(plan)
    with pytest.raises(ServiceJournalError, match='already reserved'):
        controller.launch_once(plan)
    assert transport.calls == [plan.systemd_argv]


def test_uncertain_launch_fenced(
    setup: tuple[ServiceController, ScriptedTransport, SandboxInputs],
) -> None:
    controller, transport, inputs = setup
    transport.fail = True
    plan = build_sandbox_plan(inputs)
    with pytest.raises(OSError, match='simulated uncertain transport outcome'):
        controller.launch_once(plan)
    assert controller.inspect(plan.unit).state == ServiceState.LAUNCH_UNCERTAIN  # type: ignore[union-attr]
    with pytest.raises(ServiceJournalError):
        controller.launch_once(plan)
    assert len(transport.calls) == 1


def test_unverified_stop_keeps_uncertainty(
    setup: tuple[ServiceController, ScriptedTransport, SandboxInputs],
) -> None:
    controller, transport, inputs = setup
    plan = build_sandbox_plan(inputs)
    controller.launch_once(plan)
    transport.inactive = False
    with pytest.raises(ServiceControlError, match='unverified'):
        controller.stop_once(plan)
    assert controller.inspect(plan.unit).state == ServiceState.STOP_UNCERTAIN  # type: ignore[union-attr]
    with pytest.raises(ServiceControlError):
        controller.stop_once(plan)


def test_crash_reopen_preserves_fence(
    setup: tuple[ServiceController, ScriptedTransport, SandboxInputs],
) -> None:
    controller, transport, inputs = setup
    plan = build_sandbox_plan(inputs)
    controller.launch_once(plan)
    recovered = ServiceController(ServiceJournal(controller.journal.path), transport)
    with pytest.raises(ServiceJournalError):
        recovered.launch_once(plan)
    assert recovered.inspect(plan.unit).state == ServiceState.RUNNING_OBSERVED  # type: ignore[union-attr]


def test_mismatched_plan_refused(
    setup: tuple[ServiceController, ScriptedTransport, SandboxInputs],
) -> None:
    controller, transport, inputs = setup
    plan = build_sandbox_plan(inputs)
    controller.launch_once(plan)
    altered = build_sandbox_plan(
        SandboxInputs(
            role='reviewer',
            unit=inputs.unit,
            workspace=inputs.workspace,
            evidence=inputs.evidence,
            executable=inputs.executable,
            argv=inputs.argv,
        )
    )
    with pytest.raises(ServiceControlError, match='mismatched'):
        controller.stop_once(altered)
    assert transport.calls == [plan.systemd_argv]


def test_unverified_launch_fenced(
    setup: tuple[ServiceController, ScriptedTransport, SandboxInputs],
) -> None:
    controller, transport, inputs = setup
    transport.active = False
    plan = build_sandbox_plan(inputs)
    with pytest.raises(ServiceControlError, match='unverified'):
        controller.launch_once(plan)
    assert controller.inspect(plan.unit).state == ServiceState.LAUNCH_UNCERTAIN  # type: ignore[union-attr]
    with pytest.raises(ServiceJournalError):
        controller.launch_once(plan)
