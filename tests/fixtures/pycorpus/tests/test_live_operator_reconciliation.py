"""Recovery inspection is read-only, including on failed status probes."""

from types import SimpleNamespace
from unittest.mock import Mock

from hermes_harness.operator.live_operator_reconciliation import (
    RecoveryInspectionRequest,
    inspect_operator_recovery,
)
from hermes_harness.supervised.live_supervised_recovery import Phase, RecoveryCheckpoint


def test_reconciliation_reads_without_dispatch() -> None:
    coordinator = Mock()
    coordinator.inspect_bound_service.side_effect = (True, False)
    ledger = Mock()
    ledger.read.return_value = RecoveryCheckpoint(
        phase=Phase.PENDING_HUMAN, evidence_sha256='digest', halted_reason=None
    )
    worker = SimpleNamespace(role='worker', unit='hermes-job-worker')
    reviewer = SimpleNamespace(role='reviewer', unit='hermes-job-reviewer')
    report = inspect_operator_recovery(
        coordinator, ledger, RecoveryInspectionRequest(Mock(), (worker, reviewer))
    )
    assert report.phase == Phase.PENDING_HUMAN
    assert report.services[0].active is True
    assert report.services[1].active is False
    assert 'pending' in report.guidance
    ledger.advance.assert_not_called()
    coordinator.reserve_and_launch.assert_not_called()


def test_uncertain_service_blocks_recovery_guidance() -> None:
    coordinator = Mock()
    coordinator.inspect_bound_service.side_effect = OSError('secret details')
    ledger = Mock()
    ledger.read.return_value = RecoveryCheckpoint(
        phase=Phase.WORKER_ADMITTED, evidence_sha256='digest', halted_reason=None
    )
    binding = SimpleNamespace(role='worker', unit='hermes-job-worker')
    report = inspect_operator_recovery(
        coordinator, ledger, RecoveryInspectionRequest(Mock(), (binding, binding))
    )
    assert report.requires_investigation
    assert all(item.error == 'status unverified' for item in report.services)
    assert 'secret details' not in str(report)
    assert 'do not replay' in report.guidance


def test_halted_checkpoint_never_recommends_resume() -> None:
    coordinator = Mock()
    coordinator.inspect_bound_service.return_value = False
    ledger = Mock()
    ledger.read.return_value = RecoveryCheckpoint(
        phase=Phase.HALTED, evidence_sha256='', halted_reason='cancelled'
    )
    binding = SimpleNamespace(role='worker', unit='hermes-job-worker')
    report = inspect_operator_recovery(
        coordinator, ledger, RecoveryInspectionRequest(Mock(), (binding, binding))
    )
    assert report.requires_investigation
    assert 'do not replay' in report.guidance
