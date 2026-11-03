"""Explicit operator-controlled lifecycle using systemd plans and a durable fence.

No host execution is enabled by default. Service launch and cancellation may be
performed only by an explicitly injected transport in a qualified environment.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

from hermes_harness.services.live_service_journal import ServiceState

if TYPE_CHECKING:
    from collections.abc import Sequence

    from hermes_harness.sandbox.live_sandbox_plan import SandboxPlan
    from hermes_harness.services.live_service_journal import ServiceJournal, ServiceRecord


class ServiceControlError(RuntimeError):
    """The controller cannot prove a safe service operation."""


class ServiceCommandTransport(Protocol):
    """Operator-provided systemd transport; must verify command outcome."""

    def invoke(self, argv: Sequence[str]) -> None:
        """Execute an exact, non-shell argument vector or raise."""

    def is_active(self, unit: str) -> bool:
        """Return true only after authoritative active service inspection."""

    def is_inactive(self, unit: str) -> bool:
        """Return true only after authoritative service termination verification."""


@dataclass(frozen=True, slots=True)
class ServiceController:
    """Single-use start and explicit stop; uncertainty always stays recorded."""

    journal: ServiceJournal
    transport: ServiceCommandTransport

    def launch_once(self, plan: SandboxPlan) -> ServiceRecord:
        """Persist before calling systemd; never retry an ambiguous launch."""
        record = self.journal.reserve(unit=plan.unit, role=plan.role, plan_json=plan.as_json())
        self.transport.invoke(plan.systemd_argv)
        if not self.transport.is_active(plan.unit):
            msg = 'Launch requested but active service is unverified'
            raise ServiceControlError(msg)
        self.journal.transition(
            plan.unit,
            expected=ServiceState.LAUNCH_UNCERTAIN,
            target=ServiceState.RUNNING_OBSERVED,
        )
        return record

    def stop_once(self, plan: SandboxPlan) -> None:
        """Cancel an existing matching service; never infer success from stop RPC."""
        record = self.journal.get(plan.unit)
        if record is None or record.plan_json != plan.as_json():
            msg = 'Missing or mismatched recorded service plan'
            raise ServiceControlError(msg)
        if record.state != ServiceState.RUNNING_OBSERVED:
            msg = 'Service is not in a cancellable known state'
            raise ServiceControlError(msg)
        self.journal.transition(
            plan.unit,
            expected=ServiceState.RUNNING_OBSERVED,
            target=ServiceState.STOP_UNCERTAIN,
        )
        self.transport.invoke(plan.stop_argv)
        if not self.transport.is_inactive(plan.unit):
            msg = 'Stop requested but termination is unverified'
            raise ServiceControlError(msg)
        self.journal.transition(
            plan.unit,
            expected=ServiceState.STOP_UNCERTAIN,
            target=ServiceState.STOPPED_OBSERVED,
        )

    def inspect(self, unit: str) -> ServiceRecord | None:
        """Return durable state, without submitting, stopping, or reconciling."""
        return self.journal.get(unit)

    def reconcile_stopped(self, unit: str) -> ServiceRecord:
        """Observe a prior uncertain stop without issuing stop again.

        A missing unit, mismatched state, or failed status probe remains blocked.
        This never transitions an uncertain launch into a runnable service.
        """
        record = self.journal.get(unit)
        if record is None or record.state != ServiceState.STOP_UNCERTAIN:
            msg = 'Only a previously uncertain stop can be reconciled'
            raise ServiceControlError(msg)
        if not self.transport.is_inactive(unit):
            msg = 'Stop outcome still unknown; no retry permitted'
            raise ServiceControlError(msg)
        self.journal.transition(
            unit,
            expected=ServiceState.STOP_UNCERTAIN,
            target=ServiceState.STOPPED_OBSERVED,
        )
        observed = self.journal.get(unit)
        if observed is None:
            msg = 'Reconciled service record disappeared'
            raise ServiceControlError(msg)
        return observed


# Temporary exports for legacy module import compatibility.
__all__ = [
    'Protocol',
    'ServiceCommandTransport',
    'ServiceControlError',
    'ServiceController',
    'ServiceState',
    'dataclass',
]
