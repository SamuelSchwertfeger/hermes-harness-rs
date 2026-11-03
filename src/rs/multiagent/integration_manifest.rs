//! Read-only preflight for integrating reviewed file proposals into a common HEAD.
//!
//! This module does NOT merge worktrees. It validates mutually exclusive file
//! ownership and matching baselines, then produces a deterministic integration
//! manifest for a separate, approved executor.

use std::path::Path;

use super::apply_edits::{AppliedProposal, ApplyEditsError, apply_new_files};
use super::model_edits::{EditProposal, ProposedFile};
use super::model_pipeline::ProposalReviewResult;
use super::model_review::Verdict;
use super::runtime::WorkflowError;
use crate::multiagent::runtime::fail;

/// Most candidates one integration may combine.
pub const MAX_INTEGRATION_CANDIDATES: usize = 32;
/// Length of a full SHA-1 commit id in hex.
pub const SHA1_HEX_LENGTH: usize = 40;

/// One independently reviewed unit of change with an explicit Git baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationCandidate {
    /// Node (task) identity.
    pub node_id: String,
    /// Git baseline commit the change was made against.
    pub baseline: String,
    /// Proposal and its review.
    pub result: ProposalReviewResult,
}

impl IntegrationCandidate {
    /// Candidate for `node_id` at `baseline`.
    #[must_use]
    pub fn new(node_id: &str, baseline: &str, result: ProposalReviewResult) -> Self {
        Self {
            node_id: node_id.to_owned(),
            baseline: baseline.to_owned(),
            result,
        }
    }
}

/// Deterministic plan for a separate, approved executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationManifest {
    /// Common baseline commit.
    pub baseline: String,
    /// Node identities in candidate order.
    pub nodes: Vec<String>,
    /// Proposed file paths in candidate order.
    pub files: Vec<String>,
    /// SHA-256 of each file's content, parallel to `files`.
    pub sha256: Vec<String>,
    /// Always false: nothing has been integrated.
    pub integrated: bool,
}

/// Failure of integration preflight or materialization.
#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    /// Preflight rule violation (Python `WorkflowError`).
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Application to the worktree failed.
    #[error(transparent)]
    Apply(#[from] ApplyEditsError),
}

fn validate_candidate(
    candidate: &IntegrationCandidate,
    baseline: &str,
    nodes: &[String],
) -> Result<(), WorkflowError> {
    if candidate.baseline != baseline {
        return fail("Divergent integration baselines");
    }
    if nodes.contains(&candidate.node_id) || candidate.result.proposal.task_id != candidate.node_id
    {
        return fail("Duplicate or mismatched integration task identity");
    }
    if candidate.result.review.task_id != candidate.node_id {
        return fail("Review identity does not match proposal");
    }
    if candidate.result.review.verdict != Verdict::Accept {
        return fail("Integration requires an accepting reviewer verdict");
    }
    if candidate.result.applied.is_some() {
        return fail("Already-applied proposal must follow a separate audited integration path");
    }
    Ok(())
}

fn overlaps(path: &str, prior: &str) -> bool {
    let nested = |outer: &str, inner: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.starts_with('/'))
    };
    path == prior || nested(prior, path) || nested(path, prior)
}

fn add_candidate_files(
    candidate: &IntegrationCandidate,
    paths: &mut Vec<String>,
    digests: &mut Vec<String>,
) -> Result<(), WorkflowError> {
    for file in &candidate.result.proposal.files {
        if paths.iter().any(|prior| overlaps(&file.path, prior)) {
            return fail("Overlapping integration proposals");
        }
        paths.push(file.path.clone());
        digests.push(file.sha256.clone());
    }
    Ok(())
}

/// Reject divergent baselines, incomplete review and overlapping proposals.
///
/// # Errors
///
/// Returns [`WorkflowError`] for a bad candidate count or baseline, divergent baselines,
/// duplicate or mismatched identities, a non-accepting or already applied candidate, or
/// overlapping file paths.
pub fn prepare_integration(
    candidates: &[IntegrationCandidate],
) -> Result<IntegrationManifest, WorkflowError> {
    let Some(first) = candidates
        .first()
        .filter(|_| candidates.len() <= MAX_INTEGRATION_CANDIDATES)
    else {
        return fail("Integration requires between 1 and 32 candidates");
    };
    let baseline = first.baseline.as_str();
    let hex_ok = baseline
        .chars()
        .all(|ch| matches!(ch, '0'..='9' | 'a'..='f'));
    if baseline.chars().count() != SHA1_HEX_LENGTH || !hex_ok {
        return fail("Integration baseline must be a full SHA-1 commit ID");
    }
    let mut nodes: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut digests: Vec<String> = Vec::new();
    for candidate in candidates {
        validate_candidate(candidate, baseline, &nodes)?;
        nodes.push(candidate.node_id.clone());
        add_candidate_files(candidate, &mut paths, &mut digests)?;
    }
    Ok(IntegrationManifest {
        baseline: baseline.to_owned(),
        nodes,
        files: paths,
        sha256: digests,
        integrated: false,
    })
}

/// Materialize disjoint accepted new-file proposals into one integration worktree.
///
/// The integration worktree must already be a trusted disposable checkout at
/// the common baseline. Never commits, merges or pushes a source branch.
///
/// # Errors
///
/// Returns [`IntegrationError`] without authorization, when [`prepare_integration`] rejects the
/// candidates, or when [`apply_new_files`] fails.
pub fn materialize_integration(
    workspace: &Path,
    candidates: &[IntegrationCandidate],
    authorized: bool,
) -> Result<AppliedProposal, IntegrationError> {
    if !authorized {
        return Err(
            WorkflowError::msg("Separate integration application authorization required").into(),
        );
    }
    let manifest = prepare_integration(candidates)?;
    let files: Vec<ProposedFile> = candidates
        .iter()
        .flat_map(|candidate| candidate.result.proposal.files.iter().cloned())
        .collect();
    let node = candidates
        .first()
        .map_or("", |candidate| candidate.node_id.as_str());
    let proposal = EditProposal {
        task_id: format!("integrated-{node}"),
        files,
    };
    Ok(apply_new_files(
        workspace,
        &proposal,
        &manifest.files,
        &manifest.baseline,
        true,
    )?)
}
