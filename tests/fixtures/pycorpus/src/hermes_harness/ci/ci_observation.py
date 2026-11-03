"""Offline validation of GitHub Actions observations for immutable CI candidates.

The observations must originate from a separately authenticated GitHub API client.
This module does not fetch data, authenticate its provenance, or authorize merge.
"""

from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING

from hermes_harness.core.delegation import DelegationError

if TYPE_CHECKING:
    from hermes_harness.ci.ci_request import CIRequest


class CIEvaluation(StrEnum):
    PASSING_OBSERVATION = 'passing_observation_untrusted'
    INCOMPLETE = 'incomplete'
    REJECTED = 'rejected'


@dataclass(frozen=True, slots=True)
class GitHubRunObservation:
    repository: str
    workflow_path: str
    head_sha: str
    event: str
    run_id: int
    run_attempt: int
    status: str
    conclusion: str | None


@dataclass(frozen=True, slots=True)
class GitHubJobObservation:
    run_id: int
    run_attempt: int
    name: str
    status: str
    conclusion: str | None


@dataclass(frozen=True, slots=True)
class GitHubEvidence:
    run: GitHubRunObservation
    jobs: tuple[GitHubJobObservation, ...]


@dataclass(frozen=True, slots=True)
class CIPolicy:
    repository: str
    workflow_path: str
    required_jobs: tuple[str, ...]
    allowed_events: tuple[str, ...] = ('push', 'pull_request')


def _validate_policy(policy: CIPolicy) -> None:
    if not policy.repository or policy.repository.count('/') != 1:
        msg = 'CI repository must be owner/name'
        raise DelegationError(msg)
    if not policy.workflow_path.startswith('.github/workflows/') or '..' in policy.workflow_path:
        msg = 'Invalid workflow path'
        raise DelegationError(msg)
    if not policy.required_jobs or len(policy.required_jobs) != len(set(policy.required_jobs)):
        msg = 'Required CI jobs must be nonempty and unique'
        raise DelegationError(msg)
    if not policy.allowed_events:
        msg = 'At least one CI trigger is required'
        raise DelegationError(msg)


def _evaluate_run(
    candidate: CIRequest, run: GitHubRunObservation, policy: CIPolicy
) -> CIEvaluation | None:
    """Reject mismatched run identities before checking completion."""
    identity_matches = (
        run.repository == policy.repository
        and run.workflow_path == policy.workflow_path
        and run.head_sha == candidate.commit
    )
    if not identity_matches:
        return CIEvaluation.REJECTED
    if run.event not in policy.allowed_events or run.run_id <= 0 or run.run_attempt <= 0:
        return CIEvaluation.REJECTED
    if run.status != 'completed':
        return CIEvaluation.INCOMPLETE
    if run.conclusion != 'success':
        return CIEvaluation.REJECTED
    return None


def _evaluate_jobs(evidence: GitHubEvidence, policy: CIPolicy) -> CIEvaluation:
    """Validate all provided job records against the selected run attempt."""
    if not evidence.jobs:
        return CIEvaluation.INCOMPLETE
    names: set[str] = set()
    incomplete = False
    for job in evidence.jobs:
        identity_matches = (
            job.run_id == evidence.run.run_id
            and job.run_attempt == evidence.run.run_attempt
            and bool(job.name)
            and job.name not in names
        )
        if not identity_matches:
            return CIEvaluation.REJECTED
        names.add(job.name)
        if job.status != 'completed':
            incomplete = True
        elif job.conclusion != 'success':
            return CIEvaluation.REJECTED
    if incomplete or not set(policy.required_jobs).issubset(names):
        return CIEvaluation.INCOMPLETE
    return CIEvaluation.PASSING_OBSERVATION


def evaluate_ci_observation(
    candidate: CIRequest,
    evidence: GitHubEvidence,
    policy: CIPolicy,
) -> CIEvaluation:
    """Evaluate untrusted observations; never assert authenticated provenance.

    Caller must independently fetch the current run and all paginated jobs
    from GitHub through an authenticated TLS channel and verify run attempts.
    """
    _validate_policy(policy)
    run_result = _evaluate_run(candidate, evidence.run, policy)
    if run_result is not None:
        return run_result
    return _evaluate_jobs(evidence, policy)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'CIEvaluation',
    'CIPolicy',
    'DelegationError',
    'GitHubEvidence',
    'GitHubJobObservation',
    'GitHubRunObservation',
    'StrEnum',
    '_evaluate_jobs',
    '_evaluate_run',
    '_validate_policy',
    'dataclass',
    'evaluate_ci_observation',
]
