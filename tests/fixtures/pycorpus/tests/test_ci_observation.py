"""Offline CI observation contract; no network or credentials."""

from dataclasses import replace
from types import SimpleNamespace

import pytest

from hermes_harness.ci.ci_observation import (
    CIEvaluation,
    CIPolicy,
    GitHubEvidence,
    GitHubJobObservation,
    GitHubRunObservation,
    evaluate_ci_observation,
)
from hermes_harness.core.delegation import DelegationError


@pytest.fixture
def case() -> tuple[
    SimpleNamespace, GitHubRunObservation, tuple[GitHubJobObservation, ...], CIPolicy
]:
    candidate = SimpleNamespace(commit='c' * 40)
    run = GitHubRunObservation(
        'example/harness',
        '.github/workflows/ci.yml',
        candidate.commit,
        'pull_request',
        123,
        2,
        'completed',
        'success',
    )
    jobs = (GitHubJobObservation(123, 2, 'Quality (Python 3.14)', 'completed', 'success'),)
    policy = CIPolicy('example/harness', '.github/workflows/ci.yml', ('Quality (Python 3.14)',))
    return candidate, run, jobs, policy


def test_success_is_only_untrusted_observation(case: tuple) -> None:
    candidate, run, jobs, policy = case
    assert (
        evaluate_ci_observation(candidate, GitHubEvidence(run, jobs), policy)
        is CIEvaluation.PASSING_OBSERVATION
    )


@pytest.mark.parametrize(
    ('field', 'value'),
    [
        ('repository', 'other/fork'),
        ('workflow_path', '.github/workflows/other.yml'),
        ('head_sha', '0' * 40),
        ('event', 'workflow_dispatch'),
        ('run_attempt', 0),
        ('conclusion', 'failure'),
    ],
)
def test_mismatched_run_rejected(case: tuple, field: str, value: object) -> None:
    candidate, run, jobs, policy = case
    assert (
        evaluate_ci_observation(
            candidate, GitHubEvidence(replace(run, **{field: value}), jobs), policy
        )
        is CIEvaluation.REJECTED
    )


def test_unfinished_run_is_incomplete(case: tuple) -> None:
    candidate, run, jobs, policy = case
    assert (
        evaluate_ci_observation(
            candidate, GitHubEvidence(replace(run, status='in_progress'), jobs), policy
        )
        is CIEvaluation.INCOMPLETE
    )


def test_job_attempt_mismatch_rejected(case: tuple) -> None:
    candidate, run, jobs, policy = case
    wrong = (replace(jobs[0], run_attempt=1),)
    assert (
        evaluate_ci_observation(candidate, GitHubEvidence(run, wrong), policy)
        is CIEvaluation.REJECTED
    )


def test_missing_required_job_incomplete(case: tuple) -> None:
    candidate, run, jobs, policy = case
    more = replace(policy, required_jobs=('Quality (Python 3.14)', 'Security'))
    assert (
        evaluate_ci_observation(candidate, GitHubEvidence(run, jobs), more)
        is CIEvaluation.INCOMPLETE
    )


def test_duplicate_jobs_rejected(case: tuple) -> None:
    candidate, run, jobs, policy = case
    assert (
        evaluate_ci_observation(candidate, GitHubEvidence(run, jobs + jobs), policy)
        is CIEvaluation.REJECTED
    )


def test_empty_policy_fail_closed(case: tuple) -> None:
    candidate, run, jobs, policy = case
    with pytest.raises(DelegationError, match='nonempty'):
        evaluate_ci_observation(
            candidate, GitHubEvidence(run, jobs), replace(policy, required_jobs=())
        )
