"""Integrated supervised execution around existing authoritative role/receipt ledgers.

No transport is fabricated. Callers inject a qualified Hermes RPC and isolated
service transport, and authorize each irreversible role action individually.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.agents.supervised_review_handoff import (
    HandoffSources,
    prepare_reviewer_handoff_once,
    verify_reviewer_handoff,
)
from hermes_harness.gateway.supervised_gateway_receipts import ReceiptSources
from hermes_harness.supervised.live_supervised_finalization import (
    FinalizationInputs,
    finalize_existing_roles_once,
)
from hermes_harness.supervised.live_supervised_recovery import (
    Phase,
    checkpoint_reviewer_admission,
    checkpoint_worker_admission,
    checkpoint_worker_audit,
)

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.agents.supervised_review_handoff import ReviewerHandoff
    from hermes_harness.sandbox.live_sandbox_plan import SandboxPlan
    from hermes_harness.supervised.live_execution_binding import ExecutionBinding
    from hermes_harness.supervised.live_execution_coordinator import LiveExecutionCoordinator
    from hermes_harness.supervised.live_supervised_recovery import (
        RecoveryCheckpoint,
        RecoveryIdentity,
        RecoveryLedger,
    )
    from hermes_harness.supervised.live_supervisor import LiveSupervisorInputs


class PipelineBlockedError(RuntimeError):
    """A missing authorization, evidence or checkpoint blocks advancement."""


@dataclass(frozen=True, slots=True, kw_only=True)
class PipelineInputs:
    """One disposable task and its distinct live worker/reviewer identities."""

    coordinator: LiveExecutionCoordinator
    supervisor: LiveSupervisorInputs
    worker_binding: ExecutionBinding
    reviewer_binding: ExecutionBinding
    worker_plan: SandboxPlan
    reviewer_plan: SandboxPlan
    recovery: RecoveryLedger
    identity: RecoveryIdentity
    output_dir: Path


@dataclass(frozen=True, slots=True)
class QualifiedPipeline:
    """Explicit role-by-role live pipeline with durable evidence checkpoints.

    No role is dispatched by merely creating this object. Every side-effecting
    method requires authorization for exactly that role/action.
    """

    inputs: PipelineInputs

    def _phase(self, expected: Phase) -> None:
        inputs = self.inputs
        current = inputs.recovery.initialize(inputs.identity)
        if current.phase != expected:
            msg = 'Unexpected persisted phase; inspect without retrying an uncertain gateway call'
            raise PipelineBlockedError(msg)

    def _role(self, role: str) -> None:
        inputs = self.inputs
        binding = inputs.worker_binding if role == 'worker' else inputs.reviewer_binding
        plan = inputs.worker_plan if role == 'worker' else inputs.reviewer_plan
        submission = (
            inputs.supervisor.worker_submission
            if role == 'worker'
            else inputs.supervisor.reviewer_submission
        )
        observed = (
            binding.task_id,
            binding.role,
            binding.unit,
            binding.session_id,
            binding.submission_id,
        )
        expected = (
            inputs.identity.task_id,
            role,
            plan.unit,
            submission.session_id,
            submission.submission_id,
        )
        if observed != expected or plan.role != role:
            msg = 'Service, role, session or admission identity mismatch'
            raise PipelineBlockedError(msg)
        if inputs.worker_binding.unit == inputs.reviewer_binding.unit:
            msg = 'Roles must have distinct isolated services'
            raise PipelineBlockedError(msg)

    def launch_worker(self, *, authorized: bool = False) -> None:
        self._phase(Phase.PLANNED)
        self._role('worker')
        if not authorized:
            msg = 'Explicit worker service launch authorization required'
            raise PipelineBlockedError(msg)
        self.inputs.coordinator.reserve_and_launch(
            self.inputs.worker_binding, self.inputs.worker_plan
        )

    async def submit_worker(self, *, authorized: bool = False) -> None:
        self._phase(Phase.PLANNED)
        self._role('worker')
        if not authorized:
            msg = 'Explicit worker gateway submission authorization required'
            raise PipelineBlockedError(msg)
        inputs = self.inputs
        await inputs.coordinator.submit_worker(inputs.worker_binding, inputs.supervisor)
        checkpoint_worker_admission(inputs.recovery, inputs.identity, inputs.supervisor.store)

    async def audit_worker(self) -> ReviewerHandoff:
        """Receipt reconciliation and real Git audit; never resubmit worker."""
        self._phase(Phase.WORKER_ADMITTED)
        inputs = self.inputs
        supervisor = inputs.supervisor
        handoff = await prepare_reviewer_handoff_once(
            supervisor.plan,
            supervisor.task,
            HandoffSources(ids=supervisor.ids, store=supervisor.store, rpc=supervisor.rpc),
        )
        verify_reviewer_handoff(supervisor.task, handoff)
        checkpoint_worker_audit(inputs.recovery, inputs.identity, (supervisor.task, handoff))
        return handoff

    def launch_reviewer(self, *, authorized: bool = False) -> None:
        self._phase(Phase.WORKER_AUDITED)
        self._role('reviewer')
        if not authorized:
            msg = 'Explicit independent reviewer service launch authorization required'
            raise PipelineBlockedError(msg)
        self.inputs.coordinator.reserve_and_launch(
            self.inputs.reviewer_binding, self.inputs.reviewer_plan
        )

    async def submit_reviewer(self, *, authorized: bool = False) -> None:
        self._phase(Phase.WORKER_AUDITED)
        self._role('reviewer')
        if not authorized:
            msg = 'Explicit independent reviewer submission authorization required'
            raise PipelineBlockedError(msg)
        inputs = self.inputs
        await inputs.coordinator.submit_reviewer(inputs.reviewer_binding, inputs.supervisor)
        checkpoint_reviewer_admission(inputs.recovery, inputs.identity, inputs.supervisor.store)

    async def finalize(self, handoff: ReviewerHandoff) -> RecoveryCheckpoint:
        """Reconcile existing role receipts; produce one pending human artifact."""
        self._phase(Phase.REVIEWER_ADMITTED)
        inputs = self.inputs
        supervisor = inputs.supervisor
        return await finalize_existing_roles_once(
            FinalizationInputs(
                plan=supervisor.plan,
                task=supervisor.task,
                handoff=handoff,
                admissions=supervisor.store,
                receipts=ReceiptSources(store=supervisor.store, rpc=supervisor.rpc),
                recovery=inputs.recovery,
                identity=inputs.identity,
                output_dir=inputs.output_dir,
            )
        )

    def status(self) -> RecoveryCheckpoint:
        """Observe existing checkpoint, without role dispatch or automatic resume."""
        return self.inputs.recovery.read(self.inputs.identity)
