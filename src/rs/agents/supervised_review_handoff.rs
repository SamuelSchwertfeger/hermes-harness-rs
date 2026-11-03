//! Bind a verified worker gateway completion to an audited reviewer prompt.
//!
//! Read-only with respect to the gateway: no prompt submission, session creation,
//! retry, approval, or agent execution is performed here.

use crate::util::PathLexical as _;
use std::io;

use serde_json::Value;

use super::supervised_agents::SupervisedPlan;
use crate::gateway::gateway_terminal_reconciliation::{
    AdmissionLedger, AdmissionRecordView, LedgerError, ReceiptReader, TerminalReconciliationError,
    reconcile_terminal_once,
};
use crate::gateway::supervised_gateway_receipts::RoleAdmissions;
use crate::git::worktree_delegation::{TaskWorkspace, WorktreeDelegationError, verified_audit};
use crate::util::json_digest_value;

/// Largest reviewer prompt in characters.
pub const MAX_REVIEW_PROMPT_CHARS: usize = 8_000;

/// Unbound, stale, or unsuccessful worker evidence.
#[derive(Debug, thiserror::Error)]
pub enum ReviewerHandoffError {
    /// Binding or evidence violation.
    #[error("{0}")]
    Handoff(String),
    /// Terminal reconciliation failure.
    #[error(transparent)]
    Reconcile(#[from] TerminalReconciliationError),
    /// Worktree audit failure.
    #[error(transparent)]
    Worktree(#[from] WorktreeDelegationError),
    /// The admission ledger failed.
    #[error("{0}")]
    Ledger(LedgerError),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Malformed audit JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

/// Read-only proposal; caller must separately authorize reviewer submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerHandoff {
    /// Task identity.
    pub task_id: String,
    /// Reviewer identity.
    pub reviewer_id: String,
    /// Worker submission identity.
    pub worker_submission_id: String,
    /// Worker gateway admission identity.
    pub worker_admission_id: String,
    /// SHA-256 of the canonical worktree audit record.
    pub audit_sha256: String,
    /// Prompt bound to the audit evidence.
    pub reviewer_prompt: String,
}

fn fail<T>(msg: &str) -> Result<T, ReviewerHandoffError> {
    Err(ReviewerHandoffError::Handoff(msg.to_owned()))
}

fn audit_digest(task: &TaskWorkspace) -> Result<String, ReviewerHandoffError> {
    verified_audit(task)?;
    let path = task
        .state_root
        .join(&task.brief.task_id)
        .join("worktree-audit.json");
    if path.is_symlink_lexical() || !path.is_file() {
        return fail("Worktree audit record is missing or a symlink");
    }
    // verified_audit checked this record against Git and current file contents.
    let document: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    if !document.is_object() {
        return fail("Invalid worktree audit record");
    }
    Ok(json_digest_value(&document)?)
}

/// Read-only receipt inputs bound to the planned role admissions.
pub struct HandoffSources<'a, S: ?Sized, R: ?Sized> {
    /// Role admission identities.
    pub ids: &'a RoleAdmissions,
    /// Admission ledger.
    pub store: &'a S,
    /// Receipt reader.
    pub rpc: &'a mut R,
}

fn validate_handoff_inputs<S: AdmissionLedger + ?Sized>(
    plan: &SupervisedPlan,
    task: &TaskWorkspace,
    ids: &RoleAdmissions,
    store: &S,
) -> Result<(), ReviewerHandoffError> {
    if plan.task_id != task.brief.task_id || ids.task_id != plan.task_id {
        return fail("Handoff task identity mismatch");
    }
    if plan.worker_id != task.brief.worker_id || plan.reviewer_id != task.brief.reviewer_id {
        return fail("Handoff role identity mismatch");
    }
    if ids.worker_submission_id.is_empty() || ids.reviewer_submission_id.is_empty() {
        return fail("Both role submission identities are required");
    }
    if ids.worker_submission_id == ids.reviewer_submission_id {
        return fail("Reviewer must have distinct submission identity");
    }
    let lookup = |id: &str| {
        store
            .get(id)
            .map_err(|error| ReviewerHandoffError::Ledger(Box::new(error)))
    };
    if lookup(&ids.reviewer_submission_id)?.is_some() {
        return fail("Reviewer already submitted or uncertain; do not dispatch again");
    }
    let known = lookup(&ids.worker_submission_id)?
        .is_some_and(|worker| worker.admission_id().is_some_and(|id| !id.is_empty()));
    if !known {
        return fail("Worker requires a durable admission before review");
    }
    Ok(())
}

fn reviewer_prompt(
    plan: &SupervisedPlan,
    submission_id: &str,
    admission_id: &str,
    digest: &str,
) -> Result<String, ReviewerHandoffError> {
    let prompt = format!(
        "{}\n\nImmutable worker-to-reviewer evidence binding:\nTask: {}\n\
         Worker submission: {submission_id}\nWorker admission: {admission_id}\n\
         Worker worktree audit SHA256: {digest}\n\
         Independently inspect the worktree and compare its current audit to the \
         bound digest. Do not write, push, merge, or grant approval.",
        plan.reviewer.prompt, plan.task_id,
    );
    if prompt.chars().count() > MAX_REVIEW_PROMPT_CHARS {
        return fail("Reviewer prompt exceeds bounded request size");
    }
    Ok(prompt)
}

/// Read the worker's known receipt and bind reviewer input to a fresh Git audit.
///
/// May be called after interruption for observation, but never resubmits a
/// worker or reviewer. Reviewer dispatch must use the existing one-shot ledger.
///
/// # Errors
/// The handoff identity or audit evidence is invalid, or evidence cannot be read or persisted.
pub fn prepare_reviewer_handoff_once<S, R>(
    plan: &SupervisedPlan,
    task: &TaskWorkspace,
    sources: HandoffSources<'_, S, R>,
) -> Result<ReviewerHandoff, ReviewerHandoffError>
where
    S: AdmissionLedger + ?Sized,
    R: ReceiptReader + ?Sized,
{
    let HandoffSources { ids, store, rpc } = sources;
    validate_handoff_inputs(plan, task, ids, store)?;
    let evidence = reconcile_terminal_once(rpc, store, &ids.worker_submission_id)?;
    if evidence.outcome != "completed"
        || evidence.final_response.as_deref().is_none_or(str::is_empty)
    {
        return fail("Worker requires successful nonempty terminal evidence");
    }
    let digest = audit_digest(task)?;
    let reviewer_prompt = reviewer_prompt(
        plan,
        &ids.worker_submission_id,
        &evidence.admission_id,
        &digest,
    )?;
    Ok(ReviewerHandoff {
        task_id: plan.task_id.clone(),
        reviewer_id: plan.reviewer_id.clone(),
        worker_submission_id: ids.worker_submission_id.clone(),
        worker_admission_id: evidence.admission_id,
        audit_sha256: digest,
        reviewer_prompt,
    })
}

/// Fail closed if evidence changed since the reviewer input was prepared.
///
/// # Errors
/// The handoff identity or audit evidence is invalid, or evidence cannot be read or persisted.
pub fn verify_reviewer_handoff(
    task: &TaskWorkspace,
    handoff: &ReviewerHandoff,
) -> Result<(), ReviewerHandoffError> {
    if task.brief.task_id != handoff.task_id || task.brief.reviewer_id != handoff.reviewer_id {
        return fail("Reviewer handoff identity mismatch");
    }
    if audit_digest(task)? != handoff.audit_sha256 {
        return fail("Reviewer handoff audit changed");
    }
    Ok(())
}

impl<S: ?Sized, R: ?Sized> std::fmt::Debug for HandoffSources<'_, S, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandoffSources")
            .field("ids", &self.ids)
            .finish_non_exhaustive()
    }
}
