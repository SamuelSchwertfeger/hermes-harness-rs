import asyncio
from dataclasses import dataclass, field
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING
from unittest.mock import patch

import pytest

from hermes_harness.sandbox.live_sandbox_plan import SandboxInputs, build_sandbox_plan
from hermes_harness.services.live_service_controller import ServiceController
from hermes_harness.services.live_service_journal import ServiceJournal
from hermes_harness.supervised.live_execution_binding import ExecutionBinding, ExecutionBindingStore
from hermes_harness.supervised.live_execution_coordinator import (
    LiveCoordinatorError,
    LiveExecutionCoordinator,
)

"""Fresh service observation must gate live role dispatch, not journal history."""


if TYPE_CHECKING:
    from collections.abc import Sequence


@dataclass
class FakeTransport:
    """Inert process observer with explicit failure injection."""

    active: bool = True
    unavailable: bool = False
    queries: list[str] = field(default_factory=list)

    def invoke(self, _argv: Sequence[str]) -> None:
        """Do not launch processes."""

    def is_active(self, unit: str) -> bool:
        self.queries.append(unit)
        if self.unavailable:
            msg = 'systemd unavailable'
            raise OSError(msg)
        return self.active

    def is_inactive(self, _unit: str) -> bool:
        return not self.active


def _coordinator(
    tmp_path: Path,
) -> tuple[LiveExecutionCoordinator, ExecutionBinding, FakeTransport]:
    workspace = tmp_path / 'worktree'
    evidence = tmp_path / 'evidence'
    workspace.mkdir()
    evidence.mkdir()
    plan = build_sandbox_plan(
        SandboxInputs(
            role='worker',
            unit='hermes-job-freshness',
            workspace=workspace,
            evidence=evidence,
            executable=Path('/usr/bin/env').resolve(strict=True),
            argv=('probe',),
        )
    )
    transport = FakeTransport()
    services = ServiceController(ServiceJournal(tmp_path / 'services.sqlite'), transport)
    bindings = ExecutionBindingStore(tmp_path / 'bindings.sqlite')
    binding = ExecutionBinding(
        task_id='task',
        role='worker',
        submission_id='sub',
        session_id='session',
        unit=plan.unit,
    )
    coordinator = LiveExecutionCoordinator(bindings, services)
    coordinator.reserve_and_launch(binding, plan)
    return coordinator, binding, transport


def test_status_requires_fresh_live_observation(tmp_path: Path) -> None:
    coordinator, binding, transport = _coordinator(tmp_path)
    before = coordinator.services.journal.get(binding.unit)
    assert coordinator.inspect_bound_service(binding)
    transport.active = False
    assert not coordinator.inspect_bound_service(binding)
    assert coordinator.services.journal.get(binding.unit) == before


def test_stale_running_record_blocks_worker_submission(tmp_path: Path) -> None:
    coordinator, binding, transport = _coordinator(tmp_path)
    transport.active = False
    with patch(
        'hermes_harness.supervised.live_execution_coordinator.submit_live_worker_once'
    ) as submit:
        with pytest.raises(LiveCoordinatorError, match='no longer authoritatively active'):
            asyncio.run(coordinator.submit_worker(binding, _worker_inputs(binding)))
        submit.assert_not_called()


def test_inspection_uncertainty_blocks_gateway_submission(tmp_path: Path) -> None:
    coordinator, binding, transport = _coordinator(tmp_path)
    transport.unavailable = True
    with patch(
        'hermes_harness.supervised.live_execution_coordinator.submit_live_worker_once'
    ) as submit:
        with pytest.raises(OSError, match='systemd unavailable'):
            asyncio.run(coordinator.submit_worker(binding, _worker_inputs(binding)))
        submit.assert_not_called()


def test_restart_still_checks_service(tmp_path: Path) -> None:
    coordinator, binding, transport = _coordinator(tmp_path)
    restarted = LiveExecutionCoordinator(
        ExecutionBindingStore(coordinator.bindings.path),
        ServiceController(ServiceJournal(coordinator.services.journal.path), transport),
    )
    transport.active = False
    assert not restarted.inspect_bound_service(binding)


def _worker_inputs(binding: ExecutionBinding) -> object:
    """Minimal gateway input shape: no RPC should ever be reached in these tests."""

    return SimpleNamespace(
        plan=SimpleNamespace(task_id=binding.task_id),
        worker_submission=SimpleNamespace(
            submission_id=binding.submission_id,
            session_id=binding.session_id,
        ),
    )


def test_stale_reviewer_service_blocks_reviewer_dispatch(tmp_path: Path) -> None:
    coordinator, worker_binding, transport = _coordinator(tmp_path)
    reviewer_binding = ExecutionBinding(
        task_id=worker_binding.task_id,
        role='reviewer',
        submission_id='review-sub',
        session_id='review-session',
        unit='hermes-job-review-freshness',
    )
    reviewer_plan = build_sandbox_plan(
        SandboxInputs(
            role='reviewer',
            unit=reviewer_binding.unit,
            workspace=tmp_path / 'worktree',
            evidence=tmp_path / 'evidence',
            executable=Path('/usr/bin/env').resolve(strict=True),
            argv=('probe',),
        )
    )
    coordinator.reserve_and_launch(reviewer_binding, reviewer_plan)
    transport.active = False
    with patch(
        'hermes_harness.supervised.live_execution_coordinator.submit_live_reviewer_once'
    ) as submit:
        with pytest.raises(LiveCoordinatorError, match='no longer authoritatively active'):
            asyncio.run(
                coordinator.submit_reviewer(reviewer_binding, _reviewer_inputs(reviewer_binding))
            )
        submit.assert_not_called()


def _reviewer_inputs(binding: ExecutionBinding) -> object:
    """No gateway calls should be attempted in rejection tests."""

    return SimpleNamespace(
        plan=SimpleNamespace(task_id=binding.task_id),
        reviewer_submission=SimpleNamespace(
            submission_id=binding.submission_id,
            session_id=binding.session_id,
        ),
    )
