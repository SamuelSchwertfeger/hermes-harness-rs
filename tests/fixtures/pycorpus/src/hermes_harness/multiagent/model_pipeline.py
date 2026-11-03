"""One bounded local-model coding and review transaction.

The primary supplies approved scope and a disposable worktree. The coding
model returns data-only file proposals; a distinct model reviews those bytes.
Only accepted proposals can be written, with another explicit application grant.
No shell, Git commit, merge, push, or autonomous agent tools are exposed.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.multiagent.apply_edits import apply_new_files
from hermes_harness.multiagent.local_inference import (
    LocalInferenceError,
    propose_edits,
)
from hermes_harness.multiagent.model_review import review_proposal
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.multiagent.apply_edits import AppliedProposal
    from hermes_harness.multiagent.local_inference import (
        LocalEndpoint,
        Transport,
    )
    from hermes_harness.multiagent.model_edits import EditProposal
    from hermes_harness.multiagent.model_review import ModelReview


@dataclass(frozen=True, slots=True)
class ProposalReviewResult:
    """Unapplied proposal and independent model verdict."""

    proposal: EditProposal
    review: ModelReview
    applied: AppliedProposal | None = None


def generate_and_review(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    task_id: str,
    objective: str,
    owned_paths: tuple[str, ...],
    worker: LocalEndpoint,
    reviewer: LocalEndpoint,
    transport: Transport,
    *,
    inference_authorized: bool = False,
) -> ProposalReviewResult:
    """Run serial opt-in inference, never touching the filesystem."""
    if not inference_authorized:
        msg = 'Operator authorization required before local inference'
        raise WorkflowError(msg)
    if worker.model == reviewer.model:
        msg = 'Worker and reviewer model identities must differ'
        raise LocalInferenceError(msg)
    proposal = propose_edits(worker, task_id, objective, owned_paths, transport)
    review = review_proposal(proposal, worker, reviewer, transport, objective=objective)
    return ProposalReviewResult(proposal, review)


def apply_reviewed_new_files(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    result: ProposalReviewResult,
    workspace: Path,
    owned_paths: tuple[str, ...],
    baseline: str,
    *,
    application_authorized: bool = False,
) -> ProposalReviewResult:
    """Apply reviewed proposed files only after an independent human grant."""
    if not application_authorized:
        msg = 'Operator authorization required before proposal application'
        raise WorkflowError(msg)
    if result.applied is not None:
        msg = 'Already applied; refusing duplicate application'
        raise WorkflowError(msg)
    if result.review.verdict != 'accept':
        msg = 'Reviewer did not accept the proposal'
        raise WorkflowError(msg)
    applied = apply_new_files(
        workspace,
        result.proposal,
        owned_paths,
        baseline,
        authorized=True,
    )
    return ProposalReviewResult(result.proposal, result.review, applied)
