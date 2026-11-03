"""Read-only preflight for integrating reviewed file proposals into a common HEAD.

This module does NOT merge worktrees. It validates mutually exclusive file
ownership and matching baselines, then produces a deterministic integration
manifest for a separate, approved executor.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.multiagent.apply_edits import apply_new_files
from hermes_harness.multiagent.model_edits import EditProposal
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.multiagent.apply_edits import AppliedProposal
    from hermes_harness.multiagent.model_edits import ProposedFile
    from hermes_harness.multiagent.model_pipeline import ProposalReviewResult


@dataclass(frozen=True, slots=True)
class IntegrationCandidate:
    """One independently reviewed unit of change with an explicit Git baseline."""

    node_id: str
    baseline: str
    result: ProposalReviewResult


@dataclass(frozen=True, slots=True)
class IntegrationManifest:
    baseline: str
    nodes: tuple[str, ...]
    files: tuple[str, ...]
    sha256: tuple[str, ...]
    integrated: bool = False


MAX_INTEGRATION_CANDIDATES = 32
SHA1_HEX_LENGTH = 40


def _validate_candidate(candidate: IntegrationCandidate, baseline: str, nodes: list[str]) -> None:
    if candidate.baseline != baseline:
        msg = 'Divergent integration baselines'
        raise WorkflowError(msg)
    if candidate.node_id in nodes or candidate.result.proposal.task_id != candidate.node_id:
        msg = 'Duplicate or mismatched integration task identity'
        raise WorkflowError(msg)
    if candidate.result.review.task_id != candidate.node_id:
        msg = 'Review identity does not match proposal'
        raise WorkflowError(msg)
    if candidate.result.review.verdict != 'accept':
        msg = 'Integration requires an accepting reviewer verdict'
        raise WorkflowError(msg)
    if candidate.result.applied is not None:
        msg = 'Already-applied proposal must follow a separate audited integration path'
        raise WorkflowError(msg)


def _add_candidate_files(
    candidate: IntegrationCandidate, paths: list[str], digests: list[str]
) -> None:
    for file in candidate.result.proposal.files:
        if any(
            file.path == prior
            or file.path.startswith(f'{prior}/')
            or prior.startswith(f'{file.path}/')
            for prior in paths
        ):
            msg = 'Overlapping integration proposals'
            raise WorkflowError(msg)
        paths.append(file.path)
        digests.append(file.sha256)


def prepare_integration(candidates: tuple[IntegrationCandidate, ...]) -> IntegrationManifest:
    """Reject divergent baselines, incomplete review and overlapping proposals."""
    if not candidates or len(candidates) > MAX_INTEGRATION_CANDIDATES:
        msg = 'Integration requires between 1 and 32 candidates'
        raise WorkflowError(msg)
    baseline = candidates[0].baseline
    if len(baseline) != SHA1_HEX_LENGTH or any(char not in '0123456789abcdef' for char in baseline):
        msg = 'Integration baseline must be a full SHA-1 commit ID'
        raise WorkflowError(msg)
    nodes: list[str] = []
    paths: list[str] = []
    digests: list[str] = []
    for candidate in candidates:
        _validate_candidate(candidate, baseline, nodes)
        nodes.append(candidate.node_id)
        _add_candidate_files(candidate, paths, digests)
    return IntegrationManifest(baseline, tuple(nodes), tuple(paths), tuple(digests))


def materialize_integration(
    workspace: Path,
    candidates: tuple[IntegrationCandidate, ...],
    *,
    authorized: bool = False,
) -> AppliedProposal:
    """Materialize disjoint accepted new-file proposals into one integration worktree.

    The integration worktree must already be a trusted disposable checkout at
    the common baseline. Never commits, merges or pushes a source branch.
    """
    if not authorized:
        msg = 'Separate integration application authorization required'
        raise WorkflowError(msg)
    manifest = prepare_integration(candidates)
    files: list[ProposedFile] = []
    for candidate in candidates:
        files.extend(candidate.result.proposal.files)
    proposal = EditProposal('integrated-' + candidates[0].node_id, tuple(files))
    return apply_new_files(workspace, proposal, manifest.files, manifest.baseline, authorized=True)
