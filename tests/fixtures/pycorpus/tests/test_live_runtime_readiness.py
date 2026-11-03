"""Fail-closed checklist coverage; not a live qualification test."""

from hermes_harness.services.live_runtime_readiness import (
    REQUIRED_CONTROLS,
    ReadinessFinding,
    assess_readiness,
)


def test_missing_controls_fail_closed() -> None:
    report = assess_readiness(())
    assert not report.structurally_complete
    assert set(report.missing) == REQUIRED_CONTROLS


def test_complete_report_is_not_a_runtime_attestation() -> None:
    findings = tuple(
        ReadinessFinding(control, 'host-probe-record', passed=True) for control in REQUIRED_CONTROLS
    )
    report = assess_readiness(findings)
    assert report.structurally_complete
    assert not report.failed


def test_duplicate_or_blank_evidence_fails() -> None:
    findings = (
        ReadinessFinding('cancellation', '', passed=True),
        ReadinessFinding('cancellation', 'x', passed=True),
    )
    assert 'cancellation' in assess_readiness(findings).failed
