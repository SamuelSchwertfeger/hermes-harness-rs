//! Read-only operator reconciliation of durable recovery and service observations.
//!
//! A report is not permission to retry, resume, or submit live gateway work.

use crate::services::live_service_controller::ServiceCommandTransport;
use crate::supervised::live_execution_binding::ExecutionBinding;
use crate::supervised::live_execution_coordinator::{
    LiveCoordinatorError, LiveExecutionCoordinator,
};
use crate::supervised::live_supervised_recovery::{
    Phase, RecoveryCheckpoint, RecoveryError, RecoveryIdentity, RecoveryLedger,
};

/// Read-only service status probe (the only coordinator capability used here).
pub trait ServiceInspector {
    /// Current authoritative activity of the bound service.
    fn inspect_bound_service(
        &self,
        binding: &ExecutionBinding,
    ) -> Result<bool, LiveCoordinatorError>;
}

impl<T: ServiceCommandTransport> ServiceInspector for LiveExecutionCoordinator<T> {
    fn inspect_bound_service(
        &self,
        binding: &ExecutionBinding,
    ) -> Result<bool, LiveCoordinatorError> {
        Self::inspect_bound_service(self, binding)
    }
}

/// Read-only recovery checkpoint access; no advance or halt is exposed.
pub trait RecoveryReader {
    /// Read the durable checkpoint for `identity`.
    fn read(&self, identity: &RecoveryIdentity) -> Result<RecoveryCheckpoint, RecoveryError>;
}

impl RecoveryReader for RecoveryLedger {
    fn read(&self, identity: &RecoveryIdentity) -> Result<RecoveryCheckpoint, RecoveryError> {
        Self::read(self, identity)
    }
}

/// A bounded, non-secret summary of one service's current status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceObservation {
    /// Role of the bound service.
    pub role: String,
    /// Systemd unit.
    pub unit: String,
    /// Active state, or `None` when unverified.
    pub active: Option<bool>,
    /// Fixed, non-secret error text.
    pub error: Option<String>,
}

/// Human-readable safe next action, never an executable instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorReconciliation {
    /// Durable phase.
    pub phase: Phase,
    /// One observation per bound role.
    pub services: Vec<ServiceObservation>,
    /// True when the phase is halted or any service is unverified.
    pub requires_investigation: bool,
    /// Safe next-step guidance.
    pub guidance: String,
}

fn observe<C: ServiceInspector + ?Sized>(
    coordinator: &C,
    binding: &ExecutionBinding,
) -> ServiceObservation {
    let (active, error) = coordinator.inspect_bound_service(binding).map_or_else(
        |_| (None, Some("status unverified".to_owned())),
        |active| (Some(active), None),
    );
    ServiceObservation {
        role: binding.role.clone(),
        unit: binding.unit.clone(),
        active,
        error,
    }
}

/// Identity and service bindings to inspect without authorizing dispatch.
#[derive(Debug, Clone, Copy)]
pub struct RecoveryInspectionRequest<'a> {
    /// Recovered operation identity.
    pub identity: &'a RecoveryIdentity,
    /// Worker and reviewer bindings.
    pub bindings: [&'a ExecutionBinding; 2],
}

/// Observe durable state and current services without changing either ledger.
///
/// # Errors
/// Admission, service, or recovery evidence cannot be read or validated.
pub fn inspect_operator_recovery<C, L>(
    coordinator: &C,
    ledger: &L,
    request: &RecoveryInspectionRequest<'_>,
) -> Result<OperatorReconciliation, RecoveryError>
where
    C: ServiceInspector + ?Sized,
    L: RecoveryReader + ?Sized,
{
    let checkpoint = ledger.read(request.identity)?;
    let observed: Vec<ServiceObservation> = request
        .bindings
        .iter()
        .map(|binding| observe(coordinator, binding))
        .collect();
    let uncertain = observed.iter().any(|item| item.active.is_none());
    let halted = checkpoint.phase == Phase::Halted;
    let guidance = if halted || uncertain {
        "Inspect ledgers and service status manually; do not replay submissions"
    } else if checkpoint.phase == Phase::PendingHuman {
        "Human approval remains pending; do not approve, merge, or push automatically"
    } else {
        "Inspect evidence and authorize each next role step separately"
    };
    Ok(OperatorReconciliation {
        phase: checkpoint.phase,
        services: observed,
        requires_investigation: halted || uncertain,
        guidance: guidance.to_owned(),
    })
}
