"""Independent reviewer verdict rejects untrusted or unbound output."""

import json

import pytest

from hermes_harness.supervised.live_reviewer_verdict import (
    ReviewerVerdictError,
    VerdictBinding,
    parse_reviewer_verdict,
)


def payload(**changes: object) -> str:
    data = {
        'task_id': 'task',
        'worker_submission_id': 'worker-1',
        'audit_sha256': 'a' * 64,
        'decision': 'accept',
        'findings': [],
    }
    data.update(changes)
    return json.dumps(data)


def parse(text: str) -> object:
    return parse_reviewer_verdict(
        text, VerdictBinding(task_id='task', worker_submission_id='worker-1', audit_sha256='a' * 64)
    )


def test_valid_acceptance_is_not_human_approval() -> None:
    verdict = parse(payload())
    assert verdict.decision == 'accept'


def test_changes_requested_requires_findings() -> None:
    with pytest.raises(ReviewerVerdictError, match='requires findings'):
        parse(payload(decision='changes_requested'))


@pytest.mark.parametrize(
    'change',
    [
        {'task_id': 'other'},
        {'worker_submission_id': 'other'},
        {'audit_sha256': 'b' * 64},
    ],
)
def test_identity_mismatch_rejected(change: dict[str, str]) -> None:
    with pytest.raises(ReviewerVerdictError, match='not bound'):
        parse(payload(**change))


def test_unknown_field_rejected() -> None:
    with pytest.raises(ReviewerVerdictError, match='schema'):
        parse(payload(approved=True))


def test_non_json_and_oversize_rejected() -> None:
    with pytest.raises(ReviewerVerdictError, match='not valid JSON'):
        parse('not json')
    with pytest.raises(ReviewerVerdictError, match='bounded size'):
        parse('x' * 20000)


def test_invalid_findings_rejected() -> None:
    with pytest.raises(ReviewerVerdictError, match='findings'):
        parse(payload(findings=['']))
