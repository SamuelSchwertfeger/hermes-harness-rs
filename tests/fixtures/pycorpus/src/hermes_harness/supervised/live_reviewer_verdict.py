"""Validate a bounded independent reviewer verdict; gateway success is insufficient."""

import json
from dataclasses import dataclass
from typing import Literal, cast

MAX_VERDICT_BYTES = 16384
MAX_FINDINGS = 32
MAX_FINDING_CHARACTERS = 500
Decision = Literal['accept', 'changes_requested', 'reject']


class ReviewerVerdictError(ValueError):
    """Untrusted reviewer output cannot support a human decision request."""


@dataclass(frozen=True, slots=True, kw_only=True)
class ReviewerVerdict:
    """Strict, evidence-bound reviewer response (not an approval)."""

    task_id: str
    worker_submission_id: str
    audit_sha256: str
    decision: Decision
    findings: tuple[str, ...]


@dataclass(frozen=True, slots=True, kw_only=True)
class VerdictBinding:
    """Expected identity of the Git evidence being independently reviewed."""

    task_id: str
    worker_submission_id: str
    audit_sha256: str


def _decode(response: str) -> dict[str, object]:
    if not response or len(response.encode('utf-8')) > MAX_VERDICT_BYTES:
        msg = 'Reviewer verdict is empty or exceeds bounded size'
        raise ReviewerVerdictError(msg)
    try:
        document: object = json.loads(response)
    except (ValueError, UnicodeError) as exc:
        msg = 'Reviewer verdict is not valid JSON'
        raise ReviewerVerdictError(msg) from exc
    expected = {'task_id', 'worker_submission_id', 'audit_sha256', 'decision', 'findings'}
    if not isinstance(document, dict) or set(document) != expected:
        msg = 'Reviewer verdict schema mismatch'
        raise ReviewerVerdictError(msg)
    return document


def _validate_findings(decision: object, findings: object) -> tuple[Decision, tuple[str, ...]]:
    if decision not in ('accept', 'changes_requested', 'reject'):
        msg = 'Reviewer verdict decision is invalid'
        raise ReviewerVerdictError(msg)
    if not isinstance(findings, list) or len(findings) > MAX_FINDINGS:
        msg = 'Reviewer findings must be bounded nonempty text'
        raise ReviewerVerdictError(msg)
    if not all(
        isinstance(item, str) and 0 < len(item.strip()) <= MAX_FINDING_CHARACTERS
        for item in findings
    ):
        msg = 'Reviewer findings must be bounded nonempty text'
        raise ReviewerVerdictError(msg)
    if decision != 'accept' and not findings:
        msg = 'Nonaccepting reviewer verdict requires findings'
        raise ReviewerVerdictError(msg)
    return decision, tuple(cast('list[str]', findings))


def parse_reviewer_verdict(response: str, binding: VerdictBinding) -> ReviewerVerdict:
    """Reject extra fields, identity drift, non-JSON prose, and oversized payloads."""
    document = _decode(response)
    actual = (document['task_id'], document['worker_submission_id'], document['audit_sha256'])
    if actual != (binding.task_id, binding.worker_submission_id, binding.audit_sha256):
        msg = 'Reviewer verdict is not bound to audited worker evidence'
        raise ReviewerVerdictError(msg)
    decision, findings = _validate_findings(document['decision'], document['findings'])
    return ReviewerVerdict(
        task_id=binding.task_id,
        worker_submission_id=binding.worker_submission_id,
        audit_sha256=binding.audit_sha256,
        decision=decision,
        findings=findings,
    )


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_FINDINGS',
    'MAX_FINDING_CHARACTERS',
    'MAX_VERDICT_BYTES',
    'Decision',
    'Literal',
    'ReviewerVerdict',
    'ReviewerVerdictError',
    'VerdictBinding',
    '_decode',
    '_validate_findings',
    'cast',
    'dataclass',
    'json',
    'parse_reviewer_verdict',
]
