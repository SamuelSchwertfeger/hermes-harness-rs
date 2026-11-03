//! Bind supervised gateway admission to the exact durable systemd service.
//!
//! No privilege escalation or gateway creation: operator must inject a qualified
//! service transport and a qualified gateway runtime separately.

use super::live_execution_binding::{
    ExecutionBinding, ExecutionBindingError, ExecutionBindingStore,
};
use super::live_supervisor::{
    LiveSupervisorError, LiveSupervisorInputs, submit_live_reviewer_once, submit_live_worker_once,
};
use crate::gateway::gateway_identity::Submission;
use crate::sandbox::live_sandbox_plan::SandboxPlan;
use crate::services::live_service_controller::{
    ServiceCommandTransport, ServiceControlError, ServiceController, TransportError,
};

/// Unsafe role progression or mismatched service binding.
#[derive(Debug, thiserror::Error)]
pub enum LiveCoordinatorError {
    /// A role, identity or service precondition failed.
    #[error("{0}")]
    Refused(String),
    /// The durable binding store refused.
    #[error(transparent)]
    Binding(#[from] ExecutionBindingError),
    /// The service controller refused or failed.
    #[error(transparent)]
    Service(#[from] ServiceControlError),
    /// Gateway dispatch was refused or failed.
    #[error(transparent)]
    Supervisor(#[from] LiveSupervisorError),
    /// Status transport error; blocks gateway admission.
    #[error("{0}")]
    Transport(TransportError),
}

fn refuse<T>(msg: &str) -> Result<T, LiveCoordinatorError> {
    Err(LiveCoordinatorError::Refused(msg.to_owned()))
}

/// The supervisor inputs the coordinator needs: identities plus one-shot dispatch.
pub trait SupervisedDispatch {
    /// Authorized task identity of the plan.
    fn task_id(&self) -> &str;
    /// Worker submission identity.
    fn worker_submission(&self) -> &Submission;
    /// Reviewer submission identity.
    fn reviewer_submission(&self) -> &Submission;
    /// Submit the worker exactly once.
    fn submit_worker_once(&mut self) -> Result<(), LiveSupervisorError>;
    /// Submit the reviewer exactly once.
    fn submit_reviewer_once(&mut self) -> Result<(), LiveSupervisorError>;
}

impl SupervisedDispatch for LiveSupervisorInputs<'_> {
    fn task_id(&self) -> &str {
        &self.plan.task_id
    }

    fn worker_submission(&self) -> &Submission {
        self.worker_submission
    }

    fn reviewer_submission(&self) -> &Submission {
        self.reviewer_submission
    }

    fn submit_worker_once(&mut self) -> Result<(), LiveSupervisorError> {
        submit_live_worker_once(self).map(|_| ())
    }

    fn submit_reviewer_once(&mut self) -> Result<(), LiveSupervisorError> {
        submit_live_reviewer_once(self).map(|_| ())
    }
}

/// Fail-closed role admission with durable service identity.
#[derive(Debug)]
pub struct LiveExecutionCoordinator<T: ServiceCommandTransport> {
    /// Durable role-to-service bindings.
    pub bindings: ExecutionBindingStore,
    /// Service controller and journal.
    pub services: ServiceController<T>,
}

impl<T: ServiceCommandTransport> LiveExecutionCoordinator<T> {
    /// Bind a binding store and a service controller.
    #[must_use]
    pub const fn new(bindings: ExecutionBindingStore, services: ServiceController<T>) -> Self {
        Self { bindings, services }
    }

    /// Persist binding before launch and reject role or unit mismatches.
    ///
    /// # Errors
    /// Service or admission bindings are invalid, or service, gateway, or ledger access fails.
    pub fn reserve_and_launch(
        &self,
        binding: &ExecutionBinding,
        plan: &SandboxPlan,
    ) -> Result<(), LiveCoordinatorError> {
        if plan.unit != binding.unit || plan.role.as_str() != binding.role {
            return refuse("Plan does not match task/role binding");
        }
        self.bindings.bind(binding, &self.services.journal)?;
        self.services.launch_once(plan)?;
        Ok(())
    }

    /// Dispatch only into a matching worker service already observed running.
    ///
    /// # Errors
    /// Service or admission bindings are invalid, or service, gateway, or ledger access fails.
    pub fn submit_worker<I: SupervisedDispatch + ?Sized>(
        &self,
        binding: &ExecutionBinding,
        inputs: &mut I,
    ) -> Result<(), LiveCoordinatorError> {
        if binding.role != "worker" {
            return refuse("Worker role required");
        }
        if binding.task_id != inputs.task_id() {
            return refuse("Task mismatch");
        }
        if binding.submission_id != inputs.worker_submission().submission_id {
            return refuse("Worker submission mismatch");
        }
        if binding.session_id != inputs.worker_submission().session_id {
            return refuse("Worker session mismatch");
        }
        self.require_current_service(binding)?;
        inputs.submit_worker_once()?;
        Ok(())
    }

    /// Dispatch only after existing handoff and reviewer gates validate.
    ///
    /// # Errors
    /// Service or admission bindings are invalid, or service, gateway, or ledger access fails.
    pub fn submit_reviewer<I: SupervisedDispatch + ?Sized>(
        &self,
        binding: &ExecutionBinding,
        inputs: &mut I,
    ) -> Result<(), LiveCoordinatorError> {
        if binding.role != "reviewer" {
            return refuse("Reviewer role required");
        }
        if binding.task_id != inputs.task_id() {
            return refuse("Task mismatch");
        }
        if binding.submission_id != inputs.reviewer_submission().submission_id {
            return refuse("Reviewer submission mismatch");
        }
        if binding.session_id != inputs.reviewer_submission().session_id {
            return refuse("Reviewer session mismatch");
        }
        self.require_current_service(binding)?;
        inputs.submit_reviewer_once()?;
        Ok(())
    }

    /// Require both durable identity and a fresh systemd observation.
    ///
    /// Historical `RUNNING_OBSERVED` records are not proof of current execution.
    /// Any status transport error propagates and blocks gateway admission.
    fn require_current_service(
        &self,
        binding: &ExecutionBinding,
    ) -> Result<(), LiveCoordinatorError> {
        self.bindings
            .require_running(binding, &self.services.journal)?;
        let active = self
            .services
            .transport
            .is_active(&binding.unit)
            .map_err(LiveCoordinatorError::Transport)?;
        if !active {
            return refuse("Bound service is no longer authoritatively active");
        }
        Ok(())
    }

    /// Read-only live status; never submits or mutates either ledger.
    ///
    /// # Errors
    /// Service or admission bindings are invalid, or service, gateway, or ledger access fails.
    pub fn inspect_bound_service(
        &self,
        binding: &ExecutionBinding,
    ) -> Result<bool, LiveCoordinatorError> {
        self.bindings
            .require_running(binding, &self.services.journal)?;
        self.services
            .transport
            .is_active(&binding.unit)
            .map_err(LiveCoordinatorError::Transport)
    }
}
