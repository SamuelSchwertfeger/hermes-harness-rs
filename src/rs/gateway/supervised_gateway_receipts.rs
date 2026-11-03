//! Reconcile gateway-owned worker/reviewer terminal evidence without dispatch.
//!
//! This bridge accepts existing durable admission identities only. It cannot create a
//! session, submit prompts, invoke agents, grant approval, or mutate a worktree.

use super::gateway_terminal_reconciliation::{
    AdmissionLedger, AdmissionRecordView, LedgerError, ReceiptReader, TerminalEvidence,
    TerminalReconciliationError, reconcile_terminal_once,
};
use crate::agents::supervised_agents::SupervisedPlan;

/// Largest accepted role response in characters.
pub const MAX_ROLE_RESPONSE_CHARS: usize = 40_000;

/// Missing, mismatched, or unsuccessful gateway role evidence.
#[derive(Debug, thiserror::Error)]
pub enum RoleReceiptError {
    /// Binding or evidence violation.
    #[error("{0}")]
    Receipt(String),
    /// Terminal reconciliation failure.
    #[error(transparent)]
    Reconcile(#[from] TerminalReconciliationError),
    /// The admission ledger failed.
    #[error("{0}")]
    Ledger(LedgerError),
}

/// Operator-bound immutable identities of two previously admitted turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleAdmissions {
    /// Authorized task identity.
    pub task_id: String,
    /// Worker submission identity.
    pub worker_submission_id: String,
    /// Reviewer submission identity.
    pub reviewer_submission_id: String,
}

/// Existing ledger and read-only terminal receipt transport.
pub struct ReceiptSources<'a, S: ?Sized, R: ?Sized> {
    /// Admission ledger.
    pub store: &'a S,
    /// Receipt reader.
    pub rpc: &'a mut R,
}

/// Successful receipts, not authority to approve or mutate Git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleCompletion<T> {
    /// Worker evidence.
    pub worker: TerminalEvidence<T>,
    /// Reviewer evidence.
    pub reviewer: TerminalEvidence<T>,
}

fn fail<T>(msg: &str) -> Result<T, RoleReceiptError> {
    Err(RoleReceiptError::Receipt(msg.to_owned()))
}

fn known<T: AdmissionRecordView>(record: &T) -> Option<&str> {
    record.admission_id().filter(|id| !id.is_empty())
}

fn validate<S: AdmissionLedger + ?Sized>(
    plan: &SupervisedPlan,
    ids: &RoleAdmissions,
    store: &S,
) -> Result<(), RoleReceiptError> {
    if ids.task_id.is_empty() || ids.task_id != plan.task_id {
        return fail("Admission binding must match the authorized task");
    }
    if ids.worker_submission_id.is_empty()
        || ids.reviewer_submission_id.is_empty()
        || ids.worker_submission_id == ids.reviewer_submission_id
    {
        return fail("Worker and reviewer require distinct nonblank submission IDs");
    }
    let lookup = |id: &str| {
        store
            .get(id)
            .map_err(|error| RoleReceiptError::Ledger(Box::new(error)))
    };
    let worker = lookup(&ids.worker_submission_id)?;
    let reviewer = lookup(&ids.reviewer_submission_id)?;
    let (Some(worker), Some(reviewer)) = (worker, reviewer) else {
        return fail("Both roles require preexisting durable admissions");
    };
    let (Some(worker_admission), Some(reviewer_admission)) = (known(&worker), known(&reviewer))
    else {
        return fail("Both roles require known gateway admission IDs; never resubmit");
    };
    if worker_admission == reviewer_admission || worker.session_id() == reviewer.session_id() {
        return fail("Worker and reviewer must use distinct gateway admissions and sessions");
    }
    Ok(())
}

fn successful<T>(evidence: &TerminalEvidence<T>, role: &str) -> Result<(), RoleReceiptError> {
    let response = evidence.final_response.as_deref().unwrap_or_default();
    if evidence.outcome != "completed" || response.is_empty() {
        return fail(&format!(
            "{role} did not provide a successful nonempty terminal response"
        ));
    }
    if response.chars().count() > MAX_ROLE_RESPONSE_CHARS {
        return fail(&format!(
            "{role} terminal response exceeds the bounded evidence limit"
        ));
    }
    Ok(())
}

/// Verify worker then reviewer receipt, without submitting any prompts.
///
/// Reviewer receipt is read only after the worker is terminal-successful. A
/// receipt failure preserves whatever the admission store already knows.
/// This is not proof that the reviewer actually read the worker's Git audit:
/// that binding must be established by a later execution adapter.
///
/// # Errors
/// Role identities or terminal receipts are invalid, or receipt or ledger access fails.
pub fn reconcile_supervised_roles_once<S, R>(
    plan: &SupervisedPlan,
    ids: &RoleAdmissions,
    sources: ReceiptSources<'_, S, R>,
) -> Result<RoleCompletion<S::Record>, RoleReceiptError>
where
    S: AdmissionLedger + ?Sized,
    R: ReceiptReader + ?Sized,
{
    let ReceiptSources { store, rpc } = sources;
    validate(plan, ids, store)?;
    let worker = reconcile_terminal_once(rpc, store, &ids.worker_submission_id)?;
    successful(&worker, "Worker")?;
    let reviewer = reconcile_terminal_once(rpc, store, &ids.reviewer_submission_id)?;
    successful(&reviewer, "Reviewer")?;
    Ok(RoleCompletion { worker, reviewer })
}

impl<S: ?Sized, R: ?Sized> std::fmt::Debug for ReceiptSources<'_, S, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiptSources")
            .field("injected_dependencies", &true)
            .finish_non_exhaustive()
    }
}
