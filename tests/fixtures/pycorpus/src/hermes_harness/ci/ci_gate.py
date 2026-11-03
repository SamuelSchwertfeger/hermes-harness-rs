"""Read-only, decision-time GitHub CI assessment for an immutable candidate.

The gate makes fresh GitHub REST reads using the fixed-origin client. A positive
assessment is not a human approval, merge permission, or signed attestation.
"""

import logging
from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING

from hermes_harness.ci.ci_observation import CIEvaluation, evaluate_ci_observation
from hermes_harness.ci.github_ci_reader import GitHubReadClient, fetch_github_ci_evidence
from hermes_harness.core.delegation import DelegationError

if TYPE_CHECKING:
    from hermes_harness.ci.ci_observation import CIPolicy
    from hermes_harness.ci.ci_request import CIRequest

_LOG = logging.getLogger(__name__)


class CIGateStatus(StrEnum):
    """Read-only status; no value conveys permission to mutate anything."""

    OBSERVED_PASS = 'observed_pass_not_approval'  # noqa: S105 - status, not a secret
    INCOMPLETE = 'incomplete'
    REJECTED = 'rejected'
    UNKNOWN = 'unknown'


@dataclass(frozen=True, slots=True)
class CIGateAssessment:
    """Scoped result of fresh GitHub observation, without secrets or raw JSON."""

    status: CIGateStatus
    commit: str
    tree: str
    run_id: int
    run_attempt: int | None
    repository: str
    workflow_path: str


def assess_github_ci(  # noqa: PLR0913 - stable public assessment API
    candidate: CIRequest,
    policy: CIPolicy,
    run_id: int,
    *,
    github_token: str,
) -> CIGateAssessment:
    """Re-fetch GitHub state and evaluate it without approvals or writes.

    Transport failures return UNKNOWN. Invalid/mismatched API evidence fails
    closed. An injected reader cannot be passed to this public boundary.
    """
    if not github_token:
        msg = 'A GitHub read-only token is required for a fresh CI assessment'
        raise DelegationError(msg)
    if isinstance(run_id, bool) or not isinstance(run_id, int) or run_id <= 0:
        msg = 'A positive GitHub Actions run ID is required'
        raise DelegationError(msg)
    try:
        evidence = fetch_github_ci_evidence(
            candidate,
            policy,
            run_id,
            reader=GitHubReadClient(token=github_token),
        )
        evaluation = evaluate_ci_observation(candidate, evidence, policy)
    except DelegationError:
        _LOG.warning('GitHub CI assessment unavailable or invalid for run %d', run_id)
        return CIGateAssessment(
            CIGateStatus.UNKNOWN,
            candidate.commit,
            candidate.tree,
            run_id,
            None,
            policy.repository,
            policy.workflow_path,
        )
    status = {
        CIEvaluation.PASSING_OBSERVATION: CIGateStatus.OBSERVED_PASS,
        CIEvaluation.INCOMPLETE: CIGateStatus.INCOMPLETE,
        CIEvaluation.REJECTED: CIGateStatus.REJECTED,
    }[evaluation]
    return CIGateAssessment(
        status,
        candidate.commit,
        candidate.tree,
        run_id,
        evidence.run.run_attempt,
        policy.repository,
        policy.workflow_path,
    )


# Temporary exports for legacy module import compatibility.
__all__ = [
    '_LOG',
    'CIEvaluation',
    'CIGateAssessment',
    'CIGateStatus',
    'DelegationError',
    'GitHubReadClient',
    'StrEnum',
    'assess_github_ci',
    'dataclass',
    'evaluate_ci_observation',
    'fetch_github_ci_evidence',
    'logging',
]
