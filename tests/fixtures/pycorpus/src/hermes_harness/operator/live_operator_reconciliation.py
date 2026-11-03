"""Read-only operator reconciliation of durable recovery and service observations.

A report is not permission to retry, resume, or submit live gateway work.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.supervised.live_supervised_recovery import Phase

if TYPE_CHECKING:
    from hermes_harness.supervised.live_execution_binding import ExecutionBinding
    from hermes_harness.supervised.live_execution_coordinator import LiveExecutionCoordinator
    from hermes_harness.supervised.live_supervised_recovery import RecoveryIdentity, RecoveryLedger


@dataclass(frozen=True, slots=True)
class ServiceObservation:
    """A bounded, non-secret summary of one service's current status."""

    role: str
    unit: str
    active: bool | None
    error: str | None


@dataclass(frozen=True, slots=True)
class OperatorReconciliation:
    """Human-readable safe next action, never an executable instruction."""

    phase: Phase
    services: tuple[ServiceObservation, ...]
    requires_investigation: bool
    guidance: str


def _observe(
    coordinator: LiveExecutionCoordinator,
    binding: ExecutionBinding,
) -> ServiceObservation:
    try:
        active = coordinator.inspect_bound_service(binding)
    except OSError, RuntimeError, ValueError:
        return ServiceObservation(binding.role, binding.unit, None, 'status unverified')
    return ServiceObservation(binding.role, binding.unit, active, None)


@dataclass(frozen=True, slots=True)
class RecoveryInspectionRequest:
    """Identity and service bindings to inspect without authorizing dispatch."""

    identity: RecoveryIdentity
    bindings: tuple[ExecutionBinding, ExecutionBinding]


def inspect_operator_recovery(
    coordinator: LiveExecutionCoordinator,
    ledger: RecoveryLedger,
    request: RecoveryInspectionRequest,
) -> OperatorReconciliation:
    """Observe durable state and current services without changing either ledger."""
    checkpoint = ledger.read(request.identity)
    observed = tuple(_observe(coordinator, binding) for binding in request.bindings)
    uncertain = any(item.active is None for item in observed)
    halted = checkpoint.phase == Phase.HALTED
    if halted or uncertain:
        guidance = 'Inspect ledgers and service status manually; do not replay submissions'
    elif checkpoint.phase == Phase.PENDING_HUMAN:
        guidance = 'Human approval remains pending; do not approve, merge, or push automatically'
    else:
        guidance = 'Inspect evidence and authorize each next role step separately'
    return OperatorReconciliation(checkpoint.phase, observed, halted or uncertain, guidance)
