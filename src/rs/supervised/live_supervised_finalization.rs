//! Receipt-only supervised finalization, with durable human-pending recovery.
//!
//! This module cannot launch a service, submit a prompt, approve, merge or push.
//! It consumes preexisting worker/reviewer admissions and separately audited work.

use crate::util::PathLexical as _;
use std::path::{Path, PathBuf};

use super::live_human_decision::{
    DecisionEvidence, DecisionRequest, HumanDecisionError, request_human_decision_once,
};
use super::live_supervised_recovery::{
    Phase, RecoveryCheckpoint, RecoveryError, RecoveryIdentity, RecoveryLedger,
    checkpoint_pending_human, checkpoint_reviewer_admission, checkpoint_worker_admission,
    checkpoint_worker_audit, inspect_pending_decision,
};
use crate::agents::supervised_agents::SupervisedPlan;
use crate::agents::supervised_review_handoff::{
    ReviewerHandoff, ReviewerHandoffError, verify_reviewer_handoff,
};
use crate::gateway::admission_store::{AdmissionRecord, AdmissionStore};
use crate::gateway::gateway_terminal_reconciliation::ReceiptReader;
use crate::gateway::supervised_gateway_receipts::{
    ReceiptSources, RoleAdmissions, RoleCompletion, RoleReceiptError,
    reconcile_supervised_roles_once,
};
use crate::git::worktree_delegation::TaskWorkspace;

/// Missing, conflicting or uncertain evidence prevents finalization.
#[derive(Debug, thiserror::Error)]
pub enum FinalizationBlockedError {
    /// A finalization rule refused.
    #[error("{0}")]
    Blocked(String),
    /// Recovery ledger or evidence inspection failed; propagated unchanged.
    #[error(transparent)]
    Recovery(#[from] RecoveryError),
    /// The handoff verifier failed; propagated unchanged.
    #[error(transparent)]
    Handoff(#[from] ReviewerHandoffError),
    /// Receipt reconciliation failed; propagated unchanged.
    #[error(transparent)]
    Receipt(#[from] RoleReceiptError),
    /// The human decision request failed; propagated unchanged.
    #[error(transparent)]
    Decision(#[from] HumanDecisionError),
}

fn blocked<T>(msg: &str) -> Result<T, FinalizationBlockedError> {
    Err(FinalizationBlockedError::Blocked(msg.to_owned()))
}

/// Operator-supplied existing evidence; never an execution instruction.
pub struct FinalizationInputs<'a, R: ?Sized> {
    /// Authorized plan.
    pub plan: &'a SupervisedPlan,
    /// Audited worktree.
    pub task: &'a TaskWorkspace,
    /// Immutable reviewer handoff.
    pub handoff: &'a ReviewerHandoff,
    /// Authoritative admission ledger.
    pub admissions: &'a AdmissionStore,
    /// Receipt sources; their store must be the authoritative ledger.
    pub receipts: ReceiptSources<'a, AdmissionStore, R>,
    /// Recovery checkpoint journal.
    pub recovery: &'a RecoveryLedger,
    /// Recovered operation identity.
    pub identity: &'a RecoveryIdentity,
    /// Existing, non-symlink output directory.
    pub output_dir: &'a Path,
}

/// The evidence-producing steps, injectable so tests need no live Git or gateway.
pub trait FinalizationOps<R: ?Sized> {
    /// Terminal record type carried in the decision evidence.
    type Record;

    /// Verify the immutable reviewer handoff.
    fn verify_handoff(
        &self,
        inputs: &FinalizationInputs<'_, R>,
    ) -> Result<(), FinalizationBlockedError>;

    /// Checkpoint the evidence that leaves `phase`.
    fn advance_evidence(
        &self,
        inputs: &FinalizationInputs<'_, R>,
        phase: Phase,
    ) -> Result<RecoveryCheckpoint, FinalizationBlockedError>;

    /// Fetch both terminal receipts.
    fn reconcile_roles(
        &self,
        inputs: &mut FinalizationInputs<'_, R>,
    ) -> Result<RoleCompletion<Self::Record>, FinalizationBlockedError>;

    /// Verify a pending decision file and return its digest.
    fn inspect_pending(
        &self,
        path: &Path,
        identity: &RecoveryIdentity,
    ) -> Result<String, FinalizationBlockedError>;

    /// Write the one-shot human decision request.
    fn request_decision(
        &self,
        inputs: &FinalizationInputs<'_, R>,
        evidence: &DecisionEvidence<Self::Record>,
    ) -> Result<PathBuf, FinalizationBlockedError>;

    /// Checkpoint the pending human decision.
    fn checkpoint_pending(
        &self,
        inputs: &FinalizationInputs<'_, R>,
        path: &Path,
    ) -> Result<RecoveryCheckpoint, FinalizationBlockedError>;
}

/// Production operations backed by the real verifier, ledger and gateway receipts.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveFinalizationOps;

impl<R: ReceiptReader + ?Sized> FinalizationOps<R> for LiveFinalizationOps {
    type Record = AdmissionRecord;

    fn verify_handoff(
        &self,
        inputs: &FinalizationInputs<'_, R>,
    ) -> Result<(), FinalizationBlockedError> {
        Ok(verify_reviewer_handoff(inputs.task, inputs.handoff)?)
    }

    fn advance_evidence(
        &self,
        inputs: &FinalizationInputs<'_, R>,
        phase: Phase,
    ) -> Result<RecoveryCheckpoint, FinalizationBlockedError> {
        let identity = inputs.identity;
        Ok(match phase {
            Phase::Planned => {
                checkpoint_worker_admission(inputs.recovery, identity, inputs.admissions)?
            }
            Phase::WorkerAdmitted => {
                checkpoint_worker_audit(inputs.recovery, identity, (inputs.task, inputs.handoff))?
            }
            Phase::WorkerAudited => {
                checkpoint_reviewer_admission(inputs.recovery, identity, inputs.admissions)?
            }
            _ => return blocked("No additional offline evidence progression is available"),
        })
    }

    fn reconcile_roles(
        &self,
        inputs: &mut FinalizationInputs<'_, R>,
    ) -> Result<RoleCompletion<AdmissionRecord>, FinalizationBlockedError> {
        let ids = RoleAdmissions {
            task_id: inputs.identity.task_id.clone(),
            worker_submission_id: inputs.identity.worker_submission_id.clone(),
            reviewer_submission_id: inputs.identity.reviewer_submission_id.clone(),
        };
        let sources = ReceiptSources {
            store: inputs.receipts.store,
            rpc: &mut *inputs.receipts.rpc,
        };
        Ok(reconcile_supervised_roles_once(inputs.plan, &ids, sources)?)
    }

    fn inspect_pending(
        &self,
        path: &Path,
        identity: &RecoveryIdentity,
    ) -> Result<String, FinalizationBlockedError> {
        Ok(inspect_pending_decision(path, identity)?)
    }

    fn request_decision(
        &self,
        inputs: &FinalizationInputs<'_, R>,
        evidence: &DecisionEvidence<AdmissionRecord>,
    ) -> Result<PathBuf, FinalizationBlockedError> {
        Ok(request_human_decision_once(&DecisionRequest {
            task: inputs.task,
            handoff: inputs.handoff,
            receipts: evidence,
            output_dir: inputs.output_dir,
        })?)
    }

    fn checkpoint_pending(
        &self,
        inputs: &FinalizationInputs<'_, R>,
        path: &Path,
    ) -> Result<RecoveryCheckpoint, FinalizationBlockedError> {
        Ok(checkpoint_pending_human(
            inputs.recovery,
            inputs.identity,
            path,
        )?)
    }
}

fn validate<R: ?Sized, O: FinalizationOps<R>>(
    inputs: &FinalizationInputs<'_, R>,
    ops: &O,
) -> Result<(), FinalizationBlockedError> {
    let identity = inputs.identity;
    if inputs.plan.task_id != identity.task_id || inputs.handoff.task_id != identity.task_id {
        return blocked("Task identity differs from recovered operation");
    }
    if inputs.task.brief.task_id != identity.task_id {
        return blocked("Workspace differs from recovered operation");
    }
    if inputs.handoff.worker_submission_id != identity.worker_submission_id {
        return blocked("Worker submission differs from recovered operation");
    }
    if !std::ptr::eq(inputs.receipts.store, inputs.admissions) {
        return blocked("Receipt reconciliation must use authoritative admission store");
    }
    if inputs.output_dir.is_symlink_lexical() || !inputs.output_dir.is_dir() {
        return blocked("Decision output directory unavailable or unsafe");
    }
    ops.verify_handoff(inputs)
}

/// Crash recovery: verify existing artifact, never overwrite or recreate it.
fn existing_decision<R: ?Sized, O: FinalizationOps<R>>(
    inputs: &FinalizationInputs<'_, R>,
    ops: &O,
) -> Result<RecoveryCheckpoint, FinalizationBlockedError> {
    let path = inputs.output_dir.join("human-decision-request.json");
    ops.inspect_pending(&path, inputs.identity)?;
    let current = inputs.recovery.read(inputs.identity)?;
    if current.phase == Phase::PendingHuman {
        if current.evidence_sha256 != ops.inspect_pending(&path, inputs.identity)? {
            return blocked("Pending artifact differs from persisted checkpoint");
        }
        return Ok(current);
    }
    if current.phase != Phase::ReviewerAdmitted {
        return blocked("Existing human request conflicts with recovery phase");
    }
    ops.checkpoint_pending(inputs, &path)
}

/// Advance verified checkpoints and write at most one pending human request.
///
/// Same as [`finalize_existing_roles_once`] with injected evidence operations.
///
/// # Security
/// Production callers must use [`finalize_existing_roles_once`], because permissive
/// `ops` skip the human-approval evidence checks.
///
/// # Errors
/// Handoff or terminal evidence is invalid, checkpointing fails, or decision persistence fails.
pub fn finalize_with<R: ?Sized, O: FinalizationOps<R>>(
    inputs: &mut FinalizationInputs<'_, R>,
    ops: &O,
) -> Result<RecoveryCheckpoint, FinalizationBlockedError> {
    validate(inputs, ops)?;
    let mut checkpoint = inputs.recovery.initialize(inputs.identity)?;
    if checkpoint.phase == Phase::Halted {
        return blocked("Operation halted; operator investigation required");
    }
    let decision_path = inputs.output_dir.join("human-decision-request.json");
    if decision_path.exists() || decision_path.is_symlink_lexical() {
        return existing_decision(inputs, ops);
    }
    if checkpoint.phase == Phase::PendingHuman {
        return blocked("Pending artifact missing; do not recreate after possible tampering");
    }
    for phase in [Phase::Planned, Phase::WorkerAdmitted, Phase::WorkerAudited] {
        if checkpoint.phase == phase {
            checkpoint = ops.advance_evidence(inputs, phase)?;
        }
    }
    if checkpoint.phase != Phase::ReviewerAdmitted {
        return blocked("Cannot finalize without independently admitted reviewer");
    }
    let completion = ops.reconcile_roles(inputs)?;
    ops.verify_handoff(inputs)?;
    let evidence = DecisionEvidence {
        worker: completion.worker,
        reviewer: completion.reviewer,
    };
    ops.request_decision(inputs, &evidence)?;
    ops.checkpoint_pending(inputs, &decision_path)
}

/// Advance verified checkpoints and write at most one pending human request.
///
/// An existing artifact is reconciled, never overwritten. No submission retry is
/// possible: only `prompt.receipt` can be called, for already admitted identities.
///
/// # Errors
/// Handoff or terminal evidence is invalid, checkpointing fails, or decision persistence fails.
pub fn finalize_existing_roles_once<R: ReceiptReader + ?Sized>(
    inputs: &mut FinalizationInputs<'_, R>,
) -> Result<RecoveryCheckpoint, FinalizationBlockedError> {
    finalize_with(inputs, &LiveFinalizationOps)
}

impl<R: ?Sized> std::fmt::Debug for FinalizationInputs<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FinalizationInputs")
            .field("task_id", &self.task.brief.task_id)
            .field("workspace", &self.task.workspace)
            .field("plan_task_id", &self.plan.task_id)
            .field("audit_sha256", &self.handoff.audit_sha256)
            .field("admissions", &self.admissions)
            .field("recovery", &self.recovery)
            .field("identity", &self.identity)
            .field("output_dir", &self.output_dir)
            .finish_non_exhaustive()
    }
}
