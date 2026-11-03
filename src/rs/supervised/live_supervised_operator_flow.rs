//! Explicit operator-driven live orchestration; no unattended agent dispatch.
//!
//! Each irreversible action is a separate method with an explicit authorization flag.
//! The authority for admissions is the existing admission store; recovery checkpoints
//! only summarize independently verified evidence. No replay of ambiguous RPCs.

use super::live_execution_binding::ExecutionBinding;
use super::live_execution_coordinator::{LiveCoordinatorError, LiveExecutionCoordinator};
use super::live_supervised_finalization::{
    FinalizationBlockedError, FinalizationInputs, finalize_existing_roles_once,
};
use super::live_supervised_recovery::{
    Phase, RecoveryCheckpoint, RecoveryError, checkpoint_reviewer_admission,
    checkpoint_worker_admission, checkpoint_worker_audit,
};
use super::live_supervisor::LiveSupervisorInputs;
use crate::agents::supervised_review_handoff::{
    HandoffSources, ReviewerHandoff, ReviewerHandoffError, prepare_reviewer_handoff_once,
    verify_reviewer_handoff,
};
use crate::gateway::gateway_terminal_reconciliation::ReceiptReader;
use crate::operator::live_operator_safety::{OperatorSafetyError, validate_separate_roles};
use crate::sandbox::live_sandbox_plan::SandboxPlan;
use crate::services::live_service_controller::ServiceCommandTransport;

/// A supervised step was not explicitly authorized or verifiably ordered.
#[derive(Debug, thiserror::Error)]
pub enum OperatorFlowError {
    /// An authorization, ordering or identity rule refused.
    #[error("{0}")]
    Refused(String),
    /// Recovery ledger or evidence failure; propagated unchanged.
    #[error(transparent)]
    Recovery(#[from] RecoveryError),
    /// Coordinator failure; propagated unchanged.
    #[error(transparent)]
    Coordinator(#[from] LiveCoordinatorError),
    /// Reviewer handoff failure; propagated unchanged.
    #[error(transparent)]
    Handoff(#[from] ReviewerHandoffError),
    /// Finalization failure; propagated unchanged.
    #[error(transparent)]
    Finalization(#[from] FinalizationBlockedError),
    /// Sandbox containment failure; propagated unchanged.
    #[error(transparent)]
    Safety(#[from] OperatorSafetyError),
}

fn refuse<T>(msg: &str) -> Result<T, OperatorFlowError> {
    Err(OperatorFlowError::Refused(msg.to_owned()))
}

/// The coordinator actions the flow may take; no other capability is exposed.
pub trait FlowCoordinator {
    /// Persist the binding, then launch the sandboxed service once.
    fn reserve_and_launch(
        &self,
        binding: &ExecutionBinding,
        plan: &SandboxPlan,
    ) -> Result<(), LiveCoordinatorError>;

    /// Submit the worker into its already running service.
    fn submit_worker(
        &self,
        binding: &ExecutionBinding,
        inputs: &mut LiveSupervisorInputs<'_>,
    ) -> Result<(), LiveCoordinatorError>;

    /// Submit the reviewer into its already running service.
    fn submit_reviewer(
        &self,
        binding: &ExecutionBinding,
        inputs: &mut LiveSupervisorInputs<'_>,
    ) -> Result<(), LiveCoordinatorError>;
}

impl<T: ServiceCommandTransport> FlowCoordinator for LiveExecutionCoordinator<T> {
    fn reserve_and_launch(
        &self,
        binding: &ExecutionBinding,
        plan: &SandboxPlan,
    ) -> Result<(), LiveCoordinatorError> {
        Self::reserve_and_launch(self, binding, plan)
    }

    fn submit_worker(
        &self,
        binding: &ExecutionBinding,
        inputs: &mut LiveSupervisorInputs<'_>,
    ) -> Result<(), LiveCoordinatorError> {
        Self::submit_worker(self, binding, inputs)
    }

    fn submit_reviewer(
        &self,
        binding: &ExecutionBinding,
        inputs: &mut LiveSupervisorInputs<'_>,
    ) -> Result<(), LiveCoordinatorError> {
        Self::submit_reviewer(self, binding, inputs)
    }
}

/// Exact service, admission, worktree and checkpoint identities.
pub struct OperatorFlowInputs<'a, C: ?Sized, R: ?Sized> {
    /// Service and gateway coordinator.
    pub coordinator: &'a C,
    /// Supervisor admission inputs.
    pub supervisor: LiveSupervisorInputs<'a>,
    /// Receipt-only finalization inputs.
    pub finalization: FinalizationInputs<'a, R>,
    /// Worker service binding.
    pub worker_binding: ExecutionBinding,
    /// Reviewer service binding.
    pub reviewer_binding: ExecutionBinding,
    /// Worker sandbox launch plan.
    pub worker_plan: SandboxPlan,
    /// Reviewer sandbox launch plan.
    pub reviewer_plan: SandboxPlan,
}

fn require_authorized(authorized: bool) -> Result<(), OperatorFlowError> {
    if authorized {
        Ok(())
    } else {
        refuse("Separate explicit operator authorization required for this role action")
    }
}

fn require_identity<C: ?Sized, R: ?Sized>(
    inputs: &OperatorFlowInputs<'_, C, R>,
) -> Result<(), OperatorFlowError> {
    let supervisor = &inputs.supervisor;
    let finalization = &inputs.finalization;
    if supervisor.plan != finalization.plan || supervisor.task != finalization.task {
        return refuse("Finalization and supervisor must reference the same planned worktree");
    }
    if !std::ptr::eq(supervisor.store, finalization.admissions) {
        return refuse("Both stages must use the same authoritative admission store");
    }
    if supervisor.plan.task_id != finalization.identity.task_id {
        return refuse("Recovery identity differs from supervised task");
    }
    require_role_bindings(inputs)
}

/// Validate service, role and sandbox identity binding separately.
fn require_role_bindings<C: ?Sized, R: ?Sized>(
    inputs: &OperatorFlowInputs<'_, C, R>,
) -> Result<(), OperatorFlowError> {
    let supervisor = &inputs.supervisor;
    let identity = inputs.finalization.identity;
    let expected = [
        (
            &inputs.worker_binding,
            "worker",
            &identity.worker_submission_id,
            &supervisor.worker_submission.session_id,
            &inputs.worker_plan,
        ),
        (
            &inputs.reviewer_binding,
            "reviewer",
            &identity.reviewer_submission_id,
            &supervisor.reviewer_submission.session_id,
            &inputs.reviewer_plan,
        ),
    ];
    for (binding, role, submission, session, plan) in expected {
        if binding.task_id != identity.task_id
            || binding.role != role
            || binding.submission_id != *submission
            || binding.session_id != *session
        {
            return refuse("Role service binding does not match immutable gateway identities");
        }
        if plan.unit != binding.unit || plan.role.as_str() != role {
            return refuse("Sandbox launch plan differs from bound role service");
        }
    }
    if inputs.worker_binding.unit == inputs.reviewer_binding.unit {
        return refuse("Worker and reviewer require distinct transient services");
    }
    if inputs.worker_plan.systemd_argv == inputs.reviewer_plan.systemd_argv {
        return refuse("Worker and reviewer must not share sandbox launch commands");
    }
    verify_sandbox_mounts(&inputs.worker_plan, &inputs.reviewer_plan)
}

/// Validate real Bubblewrap mount tuples, not superficial flag presence.
///
/// # Errors
/// Authorization, identity, phase, or sandbox validation fails, or an injected operation or ledger
/// fails.
pub fn verify_sandbox_mounts(
    worker_plan: &SandboxPlan,
    reviewer_plan: &SandboxPlan,
) -> Result<(), OperatorFlowError> {
    Ok(validate_separate_roles(
        &worker_plan.systemd_argv,
        &reviewer_plan.systemd_argv,
    )?)
}

/// Source of the current recovery checkpoint (Python patches `_checkpoint`).
pub trait FlowCheckpoint {
    /// Validate identities, then initialize or read the checkpoint.
    fn checkpoint<C: ?Sized, R: ?Sized>(
        &self,
        inputs: &OperatorFlowInputs<'_, C, R>,
    ) -> Result<RecoveryCheckpoint, OperatorFlowError>;
}

/// Production checkpoint source backed by the durable recovery ledger.
#[derive(Debug, Clone, Copy, Default)]
pub struct LedgerCheckpoint;

impl FlowCheckpoint for LedgerCheckpoint {
    fn checkpoint<C: ?Sized, R: ?Sized>(
        &self,
        inputs: &OperatorFlowInputs<'_, C, R>,
    ) -> Result<RecoveryCheckpoint, OperatorFlowError> {
        require_identity(inputs)?;
        Ok(inputs
            .finalization
            .recovery
            .initialize(inputs.finalization.identity)?)
    }
}

/// Ordered, individually authorized actions; never automatically chains dispatches.
pub struct SupervisedOperatorFlow<'a, C: ?Sized, R: ?Sized, K = LedgerCheckpoint> {
    /// Exact identities for every step.
    pub inputs: OperatorFlowInputs<'a, C, R>,
    checkpoints: K,
}

impl<'a, C: ?Sized, R: ?Sized> SupervisedOperatorFlow<'a, C, R> {
    /// Build a flow over the durable recovery ledger.
    #[must_use]
    pub const fn new(inputs: OperatorFlowInputs<'a, C, R>) -> Self {
        Self {
            inputs,
            checkpoints: LedgerCheckpoint,
        }
    }
}

impl<'a, C, R, K> SupervisedOperatorFlow<'a, C, R, K>
where
    C: FlowCoordinator + ?Sized,
    R: ReceiptReader + ?Sized,
    K: FlowCheckpoint,
{
    /// Build a flow with an injected checkpoint source.
    #[must_use]
    pub const fn with_checkpoints(inputs: OperatorFlowInputs<'a, C, R>, checkpoints: K) -> Self {
        Self {
            inputs,
            checkpoints,
        }
    }

    fn checkpoint(&self) -> Result<RecoveryCheckpoint, OperatorFlowError> {
        self.checkpoints.checkpoint(&self.inputs)
    }

    fn require_phase(&self, phase: Phase) -> Result<(), OperatorFlowError> {
        if self.checkpoint()?.phase == phase {
            Ok(())
        } else {
            refuse("Unexpected recovery phase; reconcile without replaying gateway requests")
        }
    }

    /// Launch the worker service; requires `authorized` and phase `Planned`.
    ///
    /// # Errors
    /// Authorization, identity, phase, or sandbox validation fails, or an injected operation or
    /// ledger fails.
    pub fn launch_worker(&self, authorized: bool) -> Result<(), OperatorFlowError> {
        require_authorized(authorized)?;
        self.require_phase(Phase::Planned)?;
        self.inputs
            .coordinator
            .reserve_and_launch(&self.inputs.worker_binding, &self.inputs.worker_plan)?;
        Ok(())
    }

    /// Submit the worker; requires `authorized` and phase `Planned`.
    ///
    /// # Errors
    /// Authorization, identity, phase, or sandbox validation fails, or an injected operation or
    /// ledger fails.
    pub fn admit_worker(&mut self, authorized: bool) -> Result<(), OperatorFlowError> {
        require_authorized(authorized)?;
        self.require_phase(Phase::Planned)?;
        self.inputs
            .coordinator
            .submit_worker(&self.inputs.worker_binding, &mut self.inputs.supervisor)?;
        checkpoint_worker_admission(
            self.inputs.finalization.recovery,
            self.inputs.finalization.identity,
            self.inputs.supervisor.store,
        )?;
        Ok(())
    }

    /// Observe an existing terminal worker and audit; no new submissions.
    ///
    /// # Errors
    /// Authorization, identity, phase, or sandbox validation fails, or an injected operation or
    /// ledger fails.
    pub fn audit_worker(&mut self) -> Result<ReviewerHandoff, OperatorFlowError> {
        self.require_phase(Phase::WorkerAdmitted)?;
        let supervisor = &mut self.inputs.supervisor;
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
            self.inputs.finalization.recovery,
            self.inputs.finalization.identity,
            (supervisor.task, &handoff),
        )?;
        Ok(handoff)
    }

    /// Launch the reviewer service; requires `authorized` and phase `WorkerAudited`.
    ///
    /// # Errors
    /// Authorization, identity, phase, or sandbox validation fails, or an injected operation or
    /// ledger fails.
    pub fn launch_reviewer(&self, authorized: bool) -> Result<(), OperatorFlowError> {
        require_authorized(authorized)?;
        self.require_phase(Phase::WorkerAudited)?;
        self.inputs
            .coordinator
            .reserve_and_launch(&self.inputs.reviewer_binding, &self.inputs.reviewer_plan)?;
        Ok(())
    }

    /// Submit the reviewer; requires `authorized` and phase `WorkerAudited`.
    ///
    /// # Errors
    /// Authorization, identity, phase, or sandbox validation fails, or an injected operation or
    /// ledger fails.
    pub fn admit_reviewer(&mut self, authorized: bool) -> Result<(), OperatorFlowError> {
        require_authorized(authorized)?;
        self.require_phase(Phase::WorkerAudited)?;
        self.inputs
            .coordinator
            .submit_reviewer(&self.inputs.reviewer_binding, &mut self.inputs.supervisor)?;
        checkpoint_reviewer_admission(
            self.inputs.finalization.recovery,
            self.inputs.finalization.identity,
            self.inputs.supervisor.store,
        )?;
        Ok(())
    }

    /// Read existing receipts, write pending human request; no submission.
    ///
    /// # Errors
    /// Authorization, identity, phase, or sandbox validation fails, or an injected operation or
    /// ledger fails.
    pub fn finalize(&mut self) -> Result<RecoveryCheckpoint, OperatorFlowError> {
        self.require_phase(Phase::ReviewerAdmitted)?;
        Ok(finalize_existing_roles_once(&mut self.inputs.finalization)?)
    }

    /// Persist a stop fence; stopping live services remains an explicit operation.
    ///
    /// # Errors
    /// Authorization, identity, phase, or sandbox validation fails, or an injected operation or
    /// ledger fails.
    pub fn halt(&self, reason: &str) -> Result<RecoveryCheckpoint, OperatorFlowError> {
        self.checkpoint()?;
        Ok(self
            .inputs
            .finalization
            .recovery
            .halt(self.inputs.finalization.identity, reason)?)
    }

    /// Read-only checkpoint; never automatically resumes or submits.
    ///
    /// # Errors
    /// Authorization, identity, phase, or sandbox validation fails, or an injected operation or
    /// ledger fails.
    pub fn status(&self) -> Result<RecoveryCheckpoint, OperatorFlowError> {
        self.checkpoint()
    }
}

impl<C: ?Sized, R: ?Sized> std::fmt::Debug for OperatorFlowInputs<'_, C, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperatorFlowInputs")
            .field("supervisor", &self.supervisor)
            .field("finalization", &self.finalization)
            .field("worker_binding", &self.worker_binding)
            .field("reviewer_binding", &self.reviewer_binding)
            .field("worker_unit", &self.worker_plan.unit)
            .field("reviewer_unit", &self.reviewer_plan.unit)
            .finish_non_exhaustive()
    }
}

impl<C: ?Sized, R: ?Sized, K> std::fmt::Debug for SupervisedOperatorFlow<'_, C, R, K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupervisedOperatorFlow")
            .field("inputs", &self.inputs)
            .finish_non_exhaustive()
    }
}
