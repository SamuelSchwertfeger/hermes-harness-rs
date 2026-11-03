"""Explicit operator-driven live orchestration; no unattended agent dispatch.

Each irreversible action is a separate method with an explicit authorization flag.
The authority for admissions is the existing admission store; recovery checkpoints
only summarize independently verified evidence. No replay of ambiguous RPCs.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.agents.supervised_review_handoff import (
    HandoffSources,
    prepare_reviewer_handoff_once,
    verify_reviewer_handoff,
)
from hermes_harness.operator.live_operator_safety import validate_separate_roles
from hermes_harness.supervised.live_supervised_finalization import finalize_existing_roles_once
from hermes_harness.supervised.live_supervised_recovery import (
    Phase,
    checkpoint_reviewer_admission,
    checkpoint_worker_admission,
    checkpoint_worker_audit,
)

if TYPE_CHECKING:
    from hermes_harness.agents.supervised_review_handoff import ReviewerHandoff
    from hermes_harness.sandbox.live_sandbox_plan import SandboxPlan
    from hermes_harness.supervised.live_execution_binding import ExecutionBinding
    from hermes_harness.supervised.live_execution_coordinator import LiveExecutionCoordinator
    from hermes_harness.supervised.live_supervised_finalization import FinalizationInputs
    from hermes_harness.supervised.live_supervised_recovery import RecoveryCheckpoint
    from hermes_harness.supervised.live_supervisor import LiveSupervisorInputs


class OperatorFlowError(RuntimeError):
    """A supervised step was not explicitly authorized or verifiably ordered."""


@dataclass(frozen=True, slots=True, kw_only=True)
class OperatorFlowInputs:
    """Exact service, admission, worktree and checkpoint identities."""

    coordinator: LiveExecutionCoordinator
    supervisor: LiveSupervisorInputs
    finalization: FinalizationInputs
    worker_binding: ExecutionBinding
    reviewer_binding: ExecutionBinding
    worker_plan: SandboxPlan
    reviewer_plan: SandboxPlan


def _require_authorized(*, authorized: bool) -> None:
    if not authorized:
        msg = 'Separate explicit operator authorization required for this role action'
        raise OperatorFlowError(msg)


def _require_identity(inputs: OperatorFlowInputs) -> None:
    supervisor = inputs.supervisor
    finalization = inputs.finalization
    identity = finalization.identity
    if supervisor.plan != finalization.plan or supervisor.task != finalization.task:
        msg = 'Finalization and supervisor must reference the same planned worktree'
        raise OperatorFlowError(msg)
    if supervisor.store is not finalization.admissions:
        msg = 'Both stages must use the same authoritative admission store'
        raise OperatorFlowError(msg)
    if supervisor.plan.task_id != identity.task_id:
        msg = 'Recovery identity differs from supervised task'
        raise OperatorFlowError(msg)
    _require_role_bindings(inputs)


def _require_role_bindings(inputs: OperatorFlowInputs) -> None:
    """Validate service, role and sandbox identity binding separately."""
    supervisor = inputs.supervisor
    identity = inputs.finalization.identity
    expected = (
        (
            inputs.worker_binding,
            'worker',
            identity.worker_submission_id,
            supervisor.worker_submission.session_id,
            inputs.worker_plan,
        ),
        (
            inputs.reviewer_binding,
            'reviewer',
            identity.reviewer_submission_id,
            supervisor.reviewer_submission.session_id,
            inputs.reviewer_plan,
        ),
    )
    for binding, role, submission, session, plan in expected:
        if (binding.task_id, binding.role, binding.submission_id, binding.session_id) != (
            identity.task_id,
            role,
            submission,
            session,
        ):
            msg = 'Role service binding does not match immutable gateway identities'
            raise OperatorFlowError(msg)
        if (plan.unit, plan.role) != (binding.unit, role):
            msg = 'Sandbox launch plan differs from bound role service'
            raise OperatorFlowError(msg)
    if inputs.worker_binding.unit == inputs.reviewer_binding.unit:
        msg = 'Worker and reviewer require distinct transient services'
        raise OperatorFlowError(msg)
    if inputs.worker_plan.systemd_argv == inputs.reviewer_plan.systemd_argv:
        msg = 'Worker and reviewer must not share sandbox launch commands'
        raise OperatorFlowError(msg)
    _verify_sandbox_mounts(inputs)


def _verify_sandbox_mounts(inputs: OperatorFlowInputs) -> None:
    """Validate real Bubblewrap mount tuples, not superficial flag presence."""
    validate_separate_roles(
        inputs.worker_plan.systemd_argv,
        inputs.reviewer_plan.systemd_argv,
    )


@dataclass(frozen=True, slots=True)
class SupervisedOperatorFlow:
    """Ordered, individually authorized actions; never automatically chains dispatches."""

    inputs: OperatorFlowInputs

    def _checkpoint(self) -> RecoveryCheckpoint:
        _require_identity(self.inputs)
        return self.inputs.finalization.recovery.initialize(self.inputs.finalization.identity)

    def _require_phase(self, phase: Phase) -> None:
        if self._checkpoint().phase != phase:
            msg = 'Unexpected recovery phase; reconcile without replaying gateway requests'
            raise OperatorFlowError(msg)

    def launch_worker(self, *, authorized: bool = False) -> None:
        _require_authorized(authorized=authorized)
        self._require_phase(Phase.PLANNED)
        self.inputs.coordinator.reserve_and_launch(
            self.inputs.worker_binding, self.inputs.worker_plan
        )

    async def admit_worker(self, *, authorized: bool = False) -> None:
        _require_authorized(authorized=authorized)
        self._require_phase(Phase.PLANNED)
        await self.inputs.coordinator.submit_worker(
            self.inputs.worker_binding, self.inputs.supervisor
        )
        checkpoint_worker_admission(
            self.inputs.finalization.recovery,
            self.inputs.finalization.identity,
            self.inputs.supervisor.store,
        )

    async def audit_worker(self) -> ReviewerHandoff:
        """Observe an existing terminal worker and audit; no new submissions."""
        self._require_phase(Phase.WORKER_ADMITTED)
        supervisor = self.inputs.supervisor
        handoff = await prepare_reviewer_handoff_once(
            supervisor.plan,
            supervisor.task,
            HandoffSources(ids=supervisor.ids, store=supervisor.store, rpc=supervisor.rpc),
        )
        verify_reviewer_handoff(supervisor.task, handoff)
        checkpoint_worker_audit(
            self.inputs.finalization.recovery,
            self.inputs.finalization.identity,
            (supervisor.task, handoff),
        )
        return handoff

    def launch_reviewer(self, *, authorized: bool = False) -> None:
        _require_authorized(authorized=authorized)
        self._require_phase(Phase.WORKER_AUDITED)
        self.inputs.coordinator.reserve_and_launch(
            self.inputs.reviewer_binding, self.inputs.reviewer_plan
        )

    async def admit_reviewer(self, *, authorized: bool = False) -> None:
        _require_authorized(authorized=authorized)
        self._require_phase(Phase.WORKER_AUDITED)
        await self.inputs.coordinator.submit_reviewer(
            self.inputs.reviewer_binding, self.inputs.supervisor
        )
        checkpoint_reviewer_admission(
            self.inputs.finalization.recovery,
            self.inputs.finalization.identity,
            self.inputs.supervisor.store,
        )

    async def finalize(self) -> RecoveryCheckpoint:
        """Read existing receipts, write pending human request; no submission."""
        self._require_phase(Phase.REVIEWER_ADMITTED)
        return await finalize_existing_roles_once(self.inputs.finalization)

    def halt(self, reason: str) -> RecoveryCheckpoint:
        """Persist a stop fence; stopping live services remains an explicit operation."""
        self._checkpoint()
        return self.inputs.finalization.recovery.halt(self.inputs.finalization.identity, reason)

    def status(self) -> RecoveryCheckpoint:
        """Read-only checkpoint; never automatically resumes or submits."""
        return self._checkpoint()


# Temporary exports for legacy module import compatibility.
__all__ = [
    'HandoffSources',
    'OperatorFlowError',
    'OperatorFlowInputs',
    'Phase',
    'SupervisedOperatorFlow',
    '_require_authorized',
    '_require_identity',
    '_require_role_bindings',
    '_verify_sandbox_mounts',
    'checkpoint_reviewer_admission',
    'checkpoint_worker_admission',
    'checkpoint_worker_audit',
    'dataclass',
    'finalize_existing_roles_once',
    'prepare_reviewer_handoff_once',
    'validate_separate_roles',
    'verify_reviewer_handoff',
]
