//! Opt-in gateway-owned role admissions; no implicit live transport or simulation.
//!
//! This is a staging API, not an end-to-end live executor. A caller must supply
//! an externally qualified sandbox, authenticated RPC and independently audited
//! worktree evidence before requesting reviewer admission.

use crate::util::PathLexical as _;
use std::io;

use crate::agents::supervised_agents::SupervisedPlan;
use crate::agents::supervised_review_handoff::{
    HandoffSources, ReviewerHandoffError, prepare_reviewer_handoff_once, verify_reviewer_handoff,
};
use crate::agents::supervised_reviewer_dispatch::{
    AuditHandoffVerifier, ReviewerDispatchBlockedError, ReviewerDispatchInputs, RuntimeBinding,
    RuntimeQualifier, submit_reviewer_once,
};
use crate::gateway::admission_store::{AdmissionRecord, AdmissionStore, AdmissionStoreError};
use crate::gateway::gateway_identity::Submission;
use crate::gateway::gateway_live_admission::{
    LiveAdmissionError, LiveAdmissionRequest, LiveAdmissionResult, submit_once,
};
use crate::gateway::gateway_readonly::GatewayRpc;
use crate::gateway::gateway_session_creation::SessionRpc;
use crate::gateway::gateway_terminal_reconciliation::{
    TerminalEvidence, TerminalReconciliationError, reconcile_terminal_once,
};
use crate::gateway::supervised_gateway_receipts::RoleAdmissions;
use crate::git::worktree_delegation::TaskWorkspace;
use crate::util::resolve_strict;

/// A missing runtime attestation or identity prevents dispatch.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LiveSupervisorError {
    /// Identity, ledger or workspace precondition failed.
    #[error("{0}")]
    Blocked(String),
    /// The runtime qualifier or reviewer dispatch gate refused.
    #[error(transparent)]
    Dispatch(#[from] ReviewerDispatchBlockedError),
    /// Reviewer handoff failure.
    #[error(transparent)]
    Handoff(#[from] ReviewerHandoffError),
    /// Live admission failure.
    #[error(transparent)]
    Admission(#[from] LiveAdmissionError),
    /// Terminal reconciliation failure.
    #[error(transparent)]
    Reconcile(#[from] TerminalReconciliationError),
    /// Admission ledger failure.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn blocked<T>(msg: &str) -> Result<T, LiveSupervisorError> {
    Err(LiveSupervisorError::Blocked(msg.to_owned()))
}

/// Authenticated RPC usable for both prompt submission and receipt reads.
pub trait SupervisorRpc: SessionRpc + GatewayRpc {}

impl<T: SessionRpc + GatewayRpc + ?Sized> SupervisorRpc for T {}

/// Explicit, externally supplied admission dependencies; no transport factory.
///
/// The qualifier has no default here: pass `&DisabledRuntimeQualifier` to keep the
/// fail-closed Python default.
pub struct LiveSupervisorInputs<'a> {
    /// Prepared worktree.
    pub task: &'a TaskWorkspace,
    /// Authorized plan.
    pub plan: &'a SupervisedPlan,
    /// Role admission identities.
    pub ids: &'a RoleAdmissions,
    /// Worker submission.
    pub worker_submission: &'a Submission,
    /// Reviewer submission.
    pub reviewer_submission: &'a Submission,
    /// Authoritative admission ledger.
    pub store: &'a AdmissionStore,
    /// Authenticated gateway RPC.
    pub rpc: &'a mut dyn SupervisorRpc,
    /// External runtime attestation.
    pub qualifier: &'a dyn RuntimeQualifier,
}

fn validate(inputs: &LiveSupervisorInputs<'_>) -> Result<(), LiveSupervisorError> {
    if inputs.plan.task_id != inputs.task.brief.task_id {
        return blocked("Plan task does not match worktree task");
    }
    if inputs.ids.task_id != inputs.plan.task_id {
        return blocked("Admission task does not match plan");
    }
    if inputs.ids.worker_submission_id != inputs.worker_submission.submission_id {
        return blocked("Worker submission ID mismatch");
    }
    if inputs.ids.reviewer_submission_id != inputs.reviewer_submission.submission_id {
        return blocked("Reviewer submission ID mismatch");
    }
    if inputs.worker_submission.session_id == inputs.reviewer_submission.session_id {
        return blocked("Separate gateway sessions required");
    }
    if inputs.worker_submission.submission_id == inputs.reviewer_submission.submission_id {
        return blocked("Separate gateway submissions required");
    }
    if inputs.task.workspace.is_symlink_lexical() || !inputs.task.workspace.is_dir() {
        return blocked("Real prepared worktree required");
    }
    Ok(())
}

/// Submit worker exactly once, with durable pre-RPC fencing and external qualification.
///
/// # Errors
/// Runtime qualification, role binding, gateway admission, or ledger access fails.
pub fn submit_live_worker_once(
    inputs: &mut LiveSupervisorInputs<'_>,
) -> Result<LiveAdmissionResult, LiveSupervisorError> {
    validate(inputs)?;
    if inputs
        .store
        .get(&inputs.worker_submission.submission_id)?
        .is_some()
    {
        return blocked("Worker already recorded or uncertain; never resubmit");
    }
    inputs.qualifier.require_qualified(&RuntimeBinding {
        task_id: inputs.plan.task_id.clone(),
        reviewer_id: inputs.plan.worker_id.clone(),
        session_id: inputs.worker_submission.session_id.clone(),
        workspace: resolve_strict(&inputs.task.workspace)?,
        audit_sha256: "pre-worker".to_owned(),
    })?;
    let request = LiveAdmissionRequest {
        submission: inputs.worker_submission.clone(),
        text: inputs.plan.worker.prompt.clone(),
    };
    Ok(submit_once(inputs.store, &request, &mut *inputs.rpc)?)
}

/// Reconcile only a known worker admission; never create or resubmit work.
///
/// # Errors
/// Runtime qualification, role binding, gateway admission, or ledger access fails.
pub fn inspect_live_worker(
    inputs: &mut LiveSupervisorInputs<'_>,
) -> Result<TerminalEvidence<AdmissionRecord>, LiveSupervisorError> {
    validate(inputs)?;
    Ok(reconcile_terminal_once(
        &mut *inputs.rpc,
        inputs.store,
        &inputs.worker_submission.submission_id,
    )?)
}

/// Dispatch a distinct reviewer only after externally recorded Git audit.
///
/// Existing worktree audit mechanisms must already have proven a real worker's
/// filesystem changes. This method never fabricates that evidence.
///
/// # Errors
/// Runtime qualification, role binding, gateway admission, or ledger access fails.
pub fn submit_live_reviewer_once(
    inputs: &mut LiveSupervisorInputs<'_>,
) -> Result<LiveAdmissionResult, LiveSupervisorError> {
    validate(inputs)?;
    let handoff = prepare_reviewer_handoff_once(
        inputs.plan,
        inputs.task,
        HandoffSources {
            ids: inputs.ids,
            store: inputs.store,
            rpc: &mut *inputs.rpc,
        },
    )?;
    verify_reviewer_handoff(inputs.task, &handoff)?;
    Ok(submit_reviewer_once(ReviewerDispatchInputs {
        task: inputs.task,
        handoff: &handoff,
        submission: inputs.reviewer_submission,
        store: inputs.store,
        rpc: &mut *inputs.rpc,
        qualifier: inputs.qualifier,
        verifier: &AuditHandoffVerifier,
    })?)
}

/// Reconcile a previously admitted reviewer without replay or approval.
///
/// # Errors
/// Runtime qualification, role binding, gateway admission, or ledger access fails.
pub fn inspect_live_reviewer(
    inputs: &mut LiveSupervisorInputs<'_>,
) -> Result<TerminalEvidence<AdmissionRecord>, LiveSupervisorError> {
    validate(inputs)?;
    Ok(reconcile_terminal_once(
        &mut *inputs.rpc,
        inputs.store,
        &inputs.reviewer_submission.submission_id,
    )?)
}

impl std::fmt::Debug for LiveSupervisorInputs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSupervisorInputs")
            .field("task_id", &self.task.brief.task_id)
            .field("workspace", &self.task.workspace)
            .field("plan_task_id", &self.plan.task_id)
            .field("ids", &self.ids)
            .field("worker_submission", &self.worker_submission)
            .field("reviewer_submission", &self.reviewer_submission)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}
