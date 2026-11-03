//! One bounded local-model coding and review transaction.
//!
//! The primary supplies approved scope and a disposable worktree. The coding
//! model returns data-only file proposals; a distinct model reviews those bytes.
//! Only accepted proposals can be written, with another explicit application grant.
//! No shell, Git commit, merge, push, or autonomous agent tools are exposed.

use std::path::Path;

use super::apply_edits::{AppliedProposal, ApplyEditsError, apply_new_files};
use super::local_inference::{LocalEndpoint, LocalInferenceError, Transport, propose_edits};
use super::model_edits::EditProposal;
use super::model_review::{ModelReview, Verdict, review_proposal};
use super::runtime::WorkflowError;

/// Failure of one pipeline step.
#[derive(Debug, thiserror::Error)]
pub enum ModelPipelineError {
    /// Missing authorization or refused application (Python `WorkflowError`).
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Local inference or review failure.
    #[error(transparent)]
    Inference(#[from] LocalInferenceError),
    /// Application to the worktree failed.
    #[error(transparent)]
    Apply(#[from] ApplyEditsError),
}

/// Unapplied proposal and independent model verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalReviewResult {
    /// The proposal the reviewer saw.
    pub proposal: EditProposal,
    /// Independent verdict.
    pub review: ModelReview,
    /// Audit once the proposal was applied.
    pub applied: Option<AppliedProposal>,
}

impl ProposalReviewResult {
    /// Result that has not been applied.
    #[must_use]
    pub const fn new(proposal: EditProposal, review: ModelReview) -> Self {
        Self {
            proposal,
            review,
            applied: None,
        }
    }
}

fn workflow<T>(message: &str) -> Result<T, ModelPipelineError> {
    Err(WorkflowError::msg(message).into())
}

/// Run serial opt-in inference, never touching the filesystem.
///
/// # Errors
///
/// Returns [`ModelPipelineError`] without operator authorization, when worker and reviewer share
/// a model, or when proposing or reviewing fails.
pub fn generate_and_review(
    task_id: &str,
    objective: &str,
    owned_paths: &[String],
    worker: &LocalEndpoint,
    reviewer: &LocalEndpoint,
    transport: &dyn Transport,
    inference_authorized: bool,
) -> Result<ProposalReviewResult, ModelPipelineError> {
    if !inference_authorized {
        return workflow("Operator authorization required before local inference");
    }
    if worker.model == reviewer.model {
        return Err(
            LocalInferenceError::msg("Worker and reviewer model identities must differ").into(),
        );
    }
    let proposal = propose_edits(worker, task_id, objective, owned_paths, transport)?;
    let review = review_proposal(&proposal, worker, reviewer, transport, Some(objective))?;
    Ok(ProposalReviewResult::new(proposal, review))
}

/// Apply reviewed proposed files only after an independent human grant.
///
/// # Errors
///
/// Returns [`ModelPipelineError`] without application authorization, for an already applied or
/// unaccepted result, or when [`apply_new_files`] fails.
pub fn apply_reviewed_new_files(
    result: &ProposalReviewResult,
    workspace: &Path,
    owned_paths: &[String],
    baseline: &str,
    application_authorized: bool,
) -> Result<ProposalReviewResult, ModelPipelineError> {
    if !application_authorized {
        return workflow("Operator authorization required before proposal application");
    }
    if result.applied.is_some() {
        return workflow("Already applied; refusing duplicate application");
    }
    if result.review.verdict != Verdict::Accept {
        return workflow("Reviewer did not accept the proposal");
    }
    let applied = apply_new_files(workspace, &result.proposal, owned_paths, baseline, true)?;
    Ok(ProposalReviewResult {
        proposal: result.proposal.clone(),
        review: result.review.clone(),
        applied: Some(applied),
    })
}
