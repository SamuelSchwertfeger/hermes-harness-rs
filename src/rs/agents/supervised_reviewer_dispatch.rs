//! Explicitly gated, one-shot reviewer prompt submission after audited handoff.
//!
//! No gateway is created or connected here. A caller must inject an already qualified
//! runtime and an authenticated RPC. The default gate never authorizes execution.

use crate::util::PathLexical as _;
use crate::util::PyStr as _;
use std::io;
use std::path::PathBuf;

use super::supervised_review_handoff::{
    ReviewerHandoff, ReviewerHandoffError, verify_reviewer_handoff,
};
use crate::gateway::admission_store::{AdmissionStore, AdmissionStoreError};
use crate::gateway::gateway_identity::Submission;
use crate::gateway::gateway_live_admission::{
    LiveAdmissionError, LiveAdmissionRequest, LiveAdmissionResult, submit_once,
};
use crate::gateway::gateway_session_creation::SessionRpc;
use crate::git::worktree_delegation::TaskWorkspace;

/// The reviewer may not be admitted to a gateway.
#[derive(Debug, thiserror::Error)]
pub enum ReviewerDispatchBlockedError {
    /// Dispatch refused by a gate or precondition.
    #[error("{0}")]
    Blocked(String),
    /// Handoff evidence verification failed.
    #[error(transparent)]
    Handoff(#[from] ReviewerHandoffError),
    /// The admission ledger failed.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// The live admission probe failed.
    #[error(transparent)]
    Admission(#[from] LiveAdmissionError),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn blocked<T>(msg: &str) -> Result<T, ReviewerDispatchBlockedError> {
    Err(ReviewerDispatchBlockedError::Blocked(msg.to_owned()))
}

/// Exact task, reviewer, session and resolved worktree requiring attestation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBinding {
    /// Task identity.
    pub task_id: String,
    /// Reviewer identity.
    pub reviewer_id: String,
    /// Reviewer session identity.
    pub session_id: String,
    /// Resolved worktree path.
    pub workspace: PathBuf,
    /// SHA-256 of the audited worktree record.
    pub audit_sha256: String,
}

/// External, independently qualified execution and isolation gate.
pub trait RuntimeQualifier {
    /// Fail unless sandbox, gateway identity, cancellation and scope are proven.
    fn require_qualified(
        &self,
        binding: &RuntimeBinding,
    ) -> Result<(), ReviewerDispatchBlockedError>;
}

/// Safe default; no live runtime is implicitly trusted.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisabledRuntimeQualifier;

impl RuntimeQualifier for DisabledRuntimeQualifier {
    fn require_qualified(
        &self,
        _binding: &RuntimeBinding,
    ) -> Result<(), ReviewerDispatchBlockedError> {
        blocked("Gateway execution disabled until runtime isolation and cancellation are qualified")
    }
}

/// Audit recheck of a reviewer handoff, injectable so tests need no Git fixtures.
pub trait HandoffVerifier {
    /// Fail closed if evidence changed since the handoff was prepared.
    fn verify(
        &self,
        task: &TaskWorkspace,
        handoff: &ReviewerHandoff,
    ) -> Result<(), ReviewerHandoffError>;
}

/// Default verifier backed by [`verify_reviewer_handoff`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AuditHandoffVerifier;

impl HandoffVerifier for AuditHandoffVerifier {
    fn verify(
        &self,
        task: &TaskWorkspace,
        handoff: &ReviewerHandoff,
    ) -> Result<(), ReviewerHandoffError> {
        verify_reviewer_handoff(task, handoff)
    }
}

/// Everything required for one reviewer dispatch.
pub struct ReviewerDispatchInputs<'a> {
    /// Task workspace.
    pub task: &'a TaskWorkspace,
    /// Audited handoff.
    pub handoff: &'a ReviewerHandoff,
    /// Reviewer submission identity.
    pub submission: &'a Submission,
    /// Admission ledger.
    pub store: &'a AdmissionStore,
    /// Authenticated RPC.
    pub rpc: &'a mut dyn SessionRpc,
    /// Runtime qualification gate.
    pub qualifier: &'a dyn RuntimeQualifier,
    /// Handoff audit verifier.
    pub verifier: &'a dyn HandoffVerifier,
}

///
/// # Errors
/// Runtime qualification, handoff verification, admission binding, or the gateway call fails.
fn check_binding(
    inputs: &ReviewerDispatchInputs<'_>,
) -> Result<RuntimeBinding, ReviewerDispatchBlockedError> {
    let (task, handoff, submission) = (inputs.task, inputs.handoff, inputs.submission);
    if submission.session_id.py_strip().is_empty() || submission.submission_id.py_strip().is_empty()
    {
        return blocked("Reviewer session and submission identities are required");
    }
    if submission.admission_id.is_some() {
        return blocked("Reviewer submission is already admitted; do not resubmit");
    }
    if inputs.store.get(&submission.submission_id)?.is_some() {
        return blocked("Reviewer submission already recorded or uncertain; do not resubmit");
    }
    if task.workspace.is_symlink_lexical() || !task.workspace.is_dir() {
        return blocked("Reviewer workspace must be an existing real directory");
    }
    inputs.verifier.verify(task, handoff)?;
    Ok(RuntimeBinding {
        task_id: handoff.task_id.clone(),
        reviewer_id: handoff.reviewer_id.clone(),
        session_id: submission.session_id.clone(),
        workspace: std::fs::canonicalize(&task.workspace)?,
        audit_sha256: handoff.audit_sha256.clone(),
    })
}

/// Submit exactly once after independent runtime attestation and audit recheck.
///
/// The admission store fences the submission before the RPC. A transport error,
/// malformed response, or uncertainty must be reconciled, never retried.
///
/// # Errors
/// Runtime qualification, handoff verification, admission binding, or the gateway call fails.
pub fn submit_reviewer_once(
    inputs: ReviewerDispatchInputs<'_>,
) -> Result<LiveAdmissionResult, ReviewerDispatchBlockedError> {
    let binding = check_binding(&inputs)?;
    inputs.qualifier.require_qualified(&binding)?;
    // Reverify immediately before the irreversible dispatch (best-effort TOCTOU guard).
    inputs.verifier.verify(inputs.task, inputs.handoff)?;
    let request = LiveAdmissionRequest {
        submission: inputs.submission.clone(),
        text: inputs.handoff.reviewer_prompt.clone(),
    };
    let ReviewerDispatchInputs { store, rpc, .. } = inputs;
    Ok(submit_once(store, &request, rpc)?)
}

impl std::fmt::Debug for ReviewerDispatchInputs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewerDispatchInputs")
            .field("task_id", &self.task.brief.task_id)
            .field("workspace", &self.task.workspace)
            .field("audit_sha256", &self.handoff.audit_sha256)
            .field("submission", &self.submission)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}
