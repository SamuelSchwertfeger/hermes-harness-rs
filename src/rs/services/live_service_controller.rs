//! Explicit operator-controlled lifecycle using systemd plans and a durable fence.
//!
//! No host execution is enabled by default. Service launch and cancellation may be
//! performed only by an explicitly injected transport in a qualified environment.

use crate::sandbox::live_sandbox_plan::SandboxPlan;
use crate::services::live_service_journal::{
    ServiceJournal, ServiceJournalError, ServiceRecord, ServiceState,
};

/// Error returned by an injected transport.
pub type TransportError = Box<dyn std::error::Error + Send + Sync>;

/// The controller cannot prove a safe service operation.
#[derive(Debug, thiserror::Error)]
pub enum ServiceControlError {
    /// The controller refused or could not verify an operation.
    #[error("{0}")]
    Unsafe(&'static str),
    /// Durable journal failure.
    #[error(transparent)]
    Journal(#[from] ServiceJournalError),
    /// Failure raised by the injected transport.
    #[error("{0}")]
    Transport(#[source] TransportError),
}

/// Operator-provided systemd transport; must verify command outcome.
pub trait ServiceCommandTransport {
    /// Execute an exact, non-shell argument vector or fail.
    fn invoke(&self, argv: &[String]) -> Result<(), TransportError>;

    /// Return true only after authoritative active service inspection.
    fn is_active(&self, unit: &str) -> Result<bool, TransportError>;

    /// Return true only after authoritative service termination verification.
    fn is_inactive(&self, unit: &str) -> Result<bool, TransportError>;
}

impl<T: ServiceCommandTransport + ?Sized> ServiceCommandTransport for &T {
    fn invoke(&self, argv: &[String]) -> Result<(), TransportError> {
        (**self).invoke(argv)
    }

    fn is_active(&self, unit: &str) -> Result<bool, TransportError> {
        (**self).is_active(unit)
    }

    fn is_inactive(&self, unit: &str) -> Result<bool, TransportError> {
        (**self).is_inactive(unit)
    }
}

/// Single-use start and explicit stop; uncertainty always stays recorded.
#[derive(Debug)]
pub struct ServiceController<T: ServiceCommandTransport> {
    /// Durable fence.
    pub journal: ServiceJournal,
    /// Injected transport.
    pub transport: T,
}

impl<T: ServiceCommandTransport> ServiceController<T> {
    /// Bind a journal and transport.
    #[must_use]
    pub const fn new(journal: ServiceJournal, transport: T) -> Self {
        Self { journal, transport }
    }

    /// Persist before calling systemd; never retry an ambiguous launch.
    ///
    /// # Errors
    /// The service plan or journal state is invalid, or the journal or injected transport fails.
    pub fn launch_once(&self, plan: &SandboxPlan) -> Result<ServiceRecord, ServiceControlError> {
        let record = self
            .journal
            .reserve(&plan.unit, plan.role.as_str(), &plan.to_json())?;
        self.transport
            .invoke(&plan.systemd_argv)
            .map_err(ServiceControlError::Transport)?;
        if !self
            .transport
            .is_active(&plan.unit)
            .map_err(ServiceControlError::Transport)?
        {
            return Err(ServiceControlError::Unsafe(
                "Launch requested but active service is unverified",
            ));
        }
        self.journal.transition(
            &plan.unit,
            ServiceState::LaunchUncertain,
            ServiceState::RunningObserved,
        )?;
        Ok(record)
    }

    /// Cancel an existing matching service; never infer success from stop RPC.
    ///
    /// # Errors
    /// The service plan or journal state is invalid, or the journal or injected transport fails.
    pub fn stop_once(&self, plan: &SandboxPlan) -> Result<(), ServiceControlError> {
        let record = self.journal.get(&plan.unit)?;
        let Some(record) = record.filter(|r| r.plan_json == plan.to_json()) else {
            return Err(ServiceControlError::Unsafe(
                "Missing or mismatched recorded service plan",
            ));
        };
        if record.state != ServiceState::RunningObserved {
            return Err(ServiceControlError::Unsafe(
                "Service is not in a cancellable known state",
            ));
        }
        self.journal.transition(
            &plan.unit,
            ServiceState::RunningObserved,
            ServiceState::StopUncertain,
        )?;
        self.transport
            .invoke(&plan.stop_argv)
            .map_err(ServiceControlError::Transport)?;
        if !self
            .transport
            .is_inactive(&plan.unit)
            .map_err(ServiceControlError::Transport)?
        {
            return Err(ServiceControlError::Unsafe(
                "Stop requested but termination is unverified",
            ));
        }
        self.journal.transition(
            &plan.unit,
            ServiceState::StopUncertain,
            ServiceState::StoppedObserved,
        )?;
        Ok(())
    }

    /// Return durable state, without submitting, stopping, or reconciling.
    ///
    /// # Errors
    /// The service plan or journal state is invalid, or the journal or injected transport fails.
    pub fn inspect(&self, unit: &str) -> Result<Option<ServiceRecord>, ServiceJournalError> {
        self.journal.get(unit)
    }

    /// Observe a prior uncertain stop without issuing stop again.
    ///
    /// A missing unit, mismatched state, or failed status probe remains blocked.
    /// This never transitions an uncertain launch into a runnable service.
    ///
    /// # Errors
    /// The service plan or journal state is invalid, or the journal or injected transport fails.
    pub fn reconcile_stopped(&self, unit: &str) -> Result<ServiceRecord, ServiceControlError> {
        let record = self.journal.get(unit)?;
        if record.is_none_or(|r| r.state != ServiceState::StopUncertain) {
            return Err(ServiceControlError::Unsafe(
                "Only a previously uncertain stop can be reconciled",
            ));
        }
        if !self
            .transport
            .is_inactive(unit)
            .map_err(ServiceControlError::Transport)?
        {
            return Err(ServiceControlError::Unsafe(
                "Stop outcome still unknown; no retry permitted",
            ));
        }
        self.journal.transition(
            unit,
            ServiceState::StopUncertain,
            ServiceState::StoppedObserved,
        )?;
        self.journal.get(unit)?.ok_or(ServiceControlError::Unsafe(
            "Reconciled service record disappeared",
        ))
    }
}
