"""Controller and gateway dispatch are fenced by exact durable identities."""

from dataclasses import dataclass, field
from pathlib import Path

import pytest

from hermes_harness.sandbox.live_sandbox_plan import SandboxInputs, build_sandbox_plan
from hermes_harness.services.live_service_controller import ServiceController
from hermes_harness.services.live_service_journal import ServiceJournal
from hermes_harness.supervised.live_execution_binding import (
    ExecutionBinding,
    ExecutionBindingError,
    ExecutionBindingStore,
)
from hermes_harness.supervised.live_execution_coordinator import (
    LiveCoordinatorError,
    LiveExecutionCoordinator,
)


@dataclass
class FakeTransport:
    """Exercise durable launch without invoking privileged host operations."""

    calls: list[tuple[str, ...]] = field(default_factory=list)

    def invoke(self, argv: tuple[str, ...]) -> None:
        self.calls.append(argv)

    def is_active(self, _unit: str) -> bool:
        return True

    def is_inactive(self, _unit: str) -> bool:
        return True


def test_launch_bind_then_refuse_duplicate(tmp_path: Path) -> None:
    workspace = tmp_path / 'worktree'
    evidence = tmp_path / 'evidence'
    workspace.mkdir()
    evidence.mkdir()
    plan = build_sandbox_plan(
        SandboxInputs(
            role='worker',
            unit='hermes-job-coordinator',
            workspace=workspace,
            evidence=evidence,
            executable=Path('/usr/bin/env').resolve(strict=True),
            argv=('probe',),
        )
    )
    transport = FakeTransport()
    services = ServiceController(ServiceJournal(tmp_path / 'services.sqlite'), transport)
    bindings = ExecutionBindingStore(tmp_path / 'bindings.sqlite')
    coordinator = LiveExecutionCoordinator(bindings, services)
    binding = ExecutionBinding(
        task_id='task',
        role='worker',
        submission_id='sub',
        session_id='session',
        unit=plan.unit,
    )
    coordinator.reserve_and_launch(binding, plan)
    assert transport.calls == [plan.systemd_argv]
    bindings.require_running(binding, services.journal)
    restarted = LiveExecutionCoordinator(ExecutionBindingStore(bindings.path), services)
    with pytest.raises(ExecutionBindingError, match='Service unit already used or uncertain'):
        restarted.reserve_and_launch(binding, plan)
    assert transport.calls == [plan.systemd_argv]


def test_mismatched_plan_denied_before_side_effect(tmp_path: Path) -> None:
    workspace = tmp_path / 'worktree'
    evidence = tmp_path / 'evidence'
    workspace.mkdir()
    evidence.mkdir()
    plan = build_sandbox_plan(
        SandboxInputs(
            role='worker',
            unit='hermes-job-mismatch',
            workspace=workspace,
            evidence=evidence,
            executable=Path('/usr/bin/env').resolve(strict=True),
            argv=('probe',),
        )
    )
    transport = FakeTransport()
    bindings = ExecutionBindingStore(tmp_path / 'bindings.sqlite')
    coordinator = LiveExecutionCoordinator(
        bindings,
        ServiceController(ServiceJournal(tmp_path / 'services.sqlite'), transport),
    )
    wrong = ExecutionBinding(
        task_id='task',
        role='reviewer',
        submission_id='sub',
        session_id='session',
        unit=plan.unit,
    )
    with pytest.raises(LiveCoordinatorError, match='Plan does not match'):
        coordinator.reserve_and_launch(wrong, plan)
    assert bindings.get('task', 'reviewer') is None
    assert transport.calls == []
