"""Explicit live runtime qualification checklist; no self-attestation as proof.

Readiness evidence must be supplied by the operator's independent host probe.
Passing this structural checklist alone never creates a runtime qualifier.
"""

from dataclasses import dataclass

REQUIRED_CONTROLS = frozenset({
    'sandbox_identity',
    'network_egress',
    'secret_scope',
    'write_scope',
    'resource_limits',
    'gateway_authentication',
    'session_binding',
    'cancellation',
    'terminal_receipt',
    'worker_evidence',
    'reviewer_read_only',
})


@dataclass(frozen=True, slots=True)
class ReadinessFinding:
    """Recorded operator assertion, not independently verified attestation."""

    control: str
    evidence_reference: str
    passed: bool


@dataclass(frozen=True, slots=True)
class ReadinessReport:
    """Coverage report; intentionally cannot authorize gateway execution."""

    missing: tuple[str, ...]
    failed: tuple[str, ...]
    structurally_complete: bool


def assess_readiness(findings: tuple[ReadinessFinding, ...]) -> ReadinessReport:
    """Fail closed for missing, duplicate, failed or empty evidence records."""
    recorded: dict[str, ReadinessFinding] = {}
    failed: set[str] = set()
    for finding in findings:
        if finding.control in recorded or finding.control not in REQUIRED_CONTROLS:
            failed.add(finding.control)
        if not finding.passed or not finding.evidence_reference.strip():
            failed.add(finding.control)
        recorded[finding.control] = finding
    missing = tuple(sorted(REQUIRED_CONTROLS - recorded.keys()))
    return ReadinessReport(missing, tuple(sorted(failed)), not missing and not failed)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'REQUIRED_CONTROLS',
    'ReadinessFinding',
    'ReadinessReport',
    'assess_readiness',
    'dataclass',
]
