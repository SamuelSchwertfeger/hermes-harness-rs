//! Integrated supervised execution around existing authoritative role/receipt ledgers.
//!
//! No transport is fabricated. Callers inject a qualified Hermes RPC and isolated
//! service transport, and authorize each irreversible role action individually.

use std::path::Path;

use super::live_execution_binding::ExecutionBinding;
use super::live_execution_coordinator::{LiveCoordinatorError, LiveExecutionCoordinator};
use super::live_supervised_finalization::{
    FinalizationBlockedError, FinalizationInputs, finalize_existing_roles_once,
};
use super::live_supervised_recovery::{
    Phase, RecoveryCheckpoint, RecoveryError, RecoveryIdentity, RecoveryLedger,
    checkpoint_reviewer_admission, checkpoint_worker_admission, checkpoint_worker_audit,
};
use super::live_supervisor::LiveSupervisorInputs;
use crate::agents::supervised_review_handoff::{
    HandoffSources, ReviewerHandoff, ReviewerHandoffError, prepare_reviewer_handoff_once,
    verify_reviewer_handoff,
};
use crate::gateway::supervised_gateway_receipts::ReceiptSources;
use crate::sandbox::live_sandbox_plan::SandboxPlan;
use crate::services::live_service_controller::ServiceCommandTransport;

/// A missing authorization, evidence or checkpoint blocks advancement.
#[derive(Debug, thiserror::Error)]
pub enum PipelineBlockedError {
    /// A pipeline rule refused.
    #[error("{0}")]
    Blocked(String),
    /// The coordinator refused or failed; propagated unchanged.
    #[error(transparent)]
    Coordinator(#[from] LiveCoordinatorError),
    /// The recovery ledger refused or failed; propagated unchanged.
    #[error(transparent)]
    Recovery(#[from] RecoveryError),
    /// The reviewer handoff failed; propagated unchanged.
    #[error(transparent)]
    Handoff(#[from] ReviewerHandoffError),
    /// Finalization refused or failed; propagated unchanged.
    #[error(transparent)]
    Finalization(#[from] FinalizationBlockedError),
}

fn blocked<T>(msg: &str) -> Result<T, PipelineBlockedError> {
    Err(PipelineBlockedError::Blocked(msg.to_owned()))
}

/// One disposable task and its distinct live worker/reviewer identities.
pub struct PipelineInputs<'a, T: ServiceCommandTransport> {
    /// Service and binding coordinator.
    pub coordinator: &'a LiveExecutionCoordinator<T>,
    /// Gateway supervisor inputs.
    pub supervisor: LiveSupervisorInputs<'a>,
    /// Worker service binding.
    pub worker_binding: &'a ExecutionBinding,
    /// Reviewer service binding.
    pub reviewer_binding: &'a ExecutionBinding,
    /// Worker sandbox plan.
    pub worker_plan: &'a SandboxPlan,
    /// Reviewer sandbox plan.
    pub reviewer_plan: &'a SandboxPlan,
    /// Recovery checkpoint journal.
    pub recovery: &'a RecoveryLedger,
    /// Recovered operation identity.
    pub identity: &'a RecoveryIdentity,
    /// Existing decision output directory.
    pub output_dir: &'a Path,
}

/// Explicit role-by-role live pipeline with durable evidence checkpoints.
///
/// No role is dispatched by merely creating this object. Every side-effecting
/// method requires authorization for exactly that role/action.
pub struct QualifiedPipeline<'a, T: ServiceCommandTransport> {
    /// Pipeline inputs.
    pub inputs: PipelineInputs<'a, T>,
}

impl<'a, T: ServiceCommandTransport> QualifiedPipeline<'a, T> {
    /// Wrap pipeline inputs; dispatches nothing.
    #[must_use]
    pub const fn new(inputs: PipelineInputs<'a, T>) -> Self {
        Self { inputs }
    }

    fn phase(&self, expected: Phase) -> Result<(), PipelineBlockedError> {
        let inputs = &self.inputs;
        let current = inputs.recovery.initialize(inputs.identity)?;
        if current.phase != expected {
            return blocked(
                "Unexpected persisted phase; inspect without retrying an uncertain gateway call",
            );
        }
        Ok(())
    }

    fn role(&self, role: &str) -> Result<(), PipelineBlockedError> {
        let inputs = &self.inputs;
        let worker = role == "worker";
        let binding = if worker {
            inputs.worker_binding
        } else {
            inputs.reviewer_binding
        };
        let plan = if worker {
            inputs.worker_plan
        } else {
            inputs.reviewer_plan
        };
        let submission = if worker {
            inputs.supervisor.worker_submission
        } else {
            inputs.supervisor.reviewer_submission
        };
        let observed = (
            &binding.task_id,
            &binding.role,
            &binding.unit,
            &binding.session_id,
            &binding.submission_id,
        );
        let expected = (
            &inputs.identity.task_id,
            &role.to_owned(),
            &plan.unit,
            &submission.session_id,
            &submission.submission_id,
        );
        if observed != expected || plan.role.as_str() != role {
            return blocked("Service, role, session or admission identity mismatch");
        }
        if inputs.worker_binding.unit == inputs.reviewer_binding.unit {
            return blocked("Roles must have distinct isolated services");
        }
        Ok(())
    }

    /// Launch the worker service; requires `authorized`.
    ///
    /// # Errors
    /// Authorization, phase, or role evidence is invalid, or service, gateway, or ledger operations
    /// fail.
    pub fn launch_worker(&self, authorized: bool) -> Result<(), PipelineBlockedError> {
        self.phase(Phase::Planned)?;
        self.role("worker")?;
        if !authorized {
            return blocked("Explicit worker service launch authorization required");
        }
        self.inputs
            .coordinator
            .reserve_and_launch(self.inputs.worker_binding, self.inputs.worker_plan)?;
        Ok(())
    }

    /// Submit the worker once, then checkpoint its admission; requires `authorized`.
    ///
    /// # Errors
    /// Authorization, phase, or role evidence is invalid, or service, gateway, or ledger operations
    /// fail.
    pub fn submit_worker(&mut self, authorized: bool) -> Result<(), PipelineBlockedError> {
        self.phase(Phase::Planned)?;
        self.role("worker")?;
        if !authorized {
            return blocked("Explicit worker gateway submission authorization required");
        }
        let inputs = &mut self.inputs;
        inputs
            .coordinator
            .submit_worker(inputs.worker_binding, &mut inputs.supervisor)?;
        checkpoint_worker_admission(inputs.recovery, inputs.identity, inputs.supervisor.store)?;
        Ok(())
    }

    /// Receipt reconciliation and real Git audit; never resubmit worker.
    ///
    /// # Errors
    /// Authorization, phase, or role evidence is invalid, or service, gateway, or ledger operations
    /// fail.
    pub fn audit_worker(&mut self) -> Result<ReviewerHandoff, PipelineBlockedError> {
        self.phase(Phase::WorkerAdmitted)?;
        let inputs = &mut self.inputs;
        let supervisor = &mut inputs.supervisor;
        let handoff = prepare_reviewer_handoff_once(
            supervisor.plan,
            supervisor.task,
            HandoffSources {
                ids: supervisor.ids,
                store: supervisor.store,
                rpc: &mut *supervisor.rpc,
            },
        )?;
        verify_reviewer_handoff(supervisor.task, &handoff)?;
        checkpoint_worker_audit(
            inputs.recovery,
            inputs.identity,
            (supervisor.task, &handoff),
        )?;
        Ok(handoff)
    }

    /// Launch the independent reviewer service; requires `authorized`.
    ///
    /// # Errors
    /// Authorization, phase, or role evidence is invalid, or service, gateway, or ledger operations
    /// fail.
    pub fn launch_reviewer(&self, authorized: bool) -> Result<(), PipelineBlockedError> {
        self.phase(Phase::WorkerAudited)?;
        self.role("reviewer")?;
        if !authorized {
            return blocked("Explicit independent reviewer service launch authorization required");
        }
        self.inputs
            .coordinator
            .reserve_and_launch(self.inputs.reviewer_binding, self.inputs.reviewer_plan)?;
        Ok(())
    }

    /// Submit the reviewer once, then checkpoint its admission; requires `authorized`.
    ///
    /// # Errors
    /// Authorization, phase, or role evidence is invalid, or service, gateway, or ledger operations
    /// fail.
    pub fn submit_reviewer(&mut self, authorized: bool) -> Result<(), PipelineBlockedError> {
        self.phase(Phase::WorkerAudited)?;
        self.role("reviewer")?;
        if !authorized {
            return blocked("Explicit independent reviewer submission authorization required");
        }
        let inputs = &mut self.inputs;
        inputs
            .coordinator
            .submit_reviewer(inputs.reviewer_binding, &mut inputs.supervisor)?;
        checkpoint_reviewer_admission(inputs.recovery, inputs.identity, inputs.supervisor.store)?;
        Ok(())
    }

    /// Reconcile existing role receipts; produce one pending human artifact.
    ///
    /// # Errors
    /// Authorization, phase, or role evidence is invalid, or service, gateway, or ledger operations
    /// fail.
    pub fn finalize(
        &mut self,
        handoff: &ReviewerHandoff,
    ) -> Result<RecoveryCheckpoint, PipelineBlockedError> {
        self.phase(Phase::ReviewerAdmitted)?;
        let inputs = &mut self.inputs;
        let supervisor = &mut inputs.supervisor;
        let mut finalization = FinalizationInputs {
            plan: supervisor.plan,
            task: supervisor.task,
            handoff,
            admissions: supervisor.store,
            receipts: ReceiptSources {
                store: supervisor.store,
                rpc: &mut *supervisor.rpc,
            },
            recovery: inputs.recovery,
            identity: inputs.identity,
            output_dir: inputs.output_dir,
        };
        Ok(finalize_existing_roles_once(&mut finalization)?)
    }

    /// Observe existing checkpoint, without role dispatch or automatic resume.
    ///
    /// # Errors
    /// Authorization, phase, or role evidence is invalid, or service, gateway, or ledger operations
    /// fail.
    pub fn status(&self) -> Result<RecoveryCheckpoint, PipelineBlockedError> {
        Ok(self.inputs.recovery.read(self.inputs.identity)?)
    }
}

impl<T: ServiceCommandTransport> std::fmt::Debug for PipelineInputs<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineInputs")
            .field("supervisor", &self.supervisor)
            .field("worker_binding", &self.worker_binding)
            .field("reviewer_binding", &self.reviewer_binding)
            .field("worker_unit", &self.worker_plan.unit)
            .field("reviewer_unit", &self.reviewer_plan.unit)
            .field("recovery", &self.recovery)
            .field("identity", &self.identity)
            .field("output_dir", &self.output_dir)
            .finish_non_exhaustive()
    }
}

impl<T: ServiceCommandTransport> std::fmt::Debug for QualifiedPipeline<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QualifiedPipeline")
            .field("inputs", &self.inputs)
            .finish_non_exhaustive()
    }
}
