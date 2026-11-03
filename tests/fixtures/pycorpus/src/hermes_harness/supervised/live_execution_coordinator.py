"""Bind supervised gateway admission to the exact durable systemd service.

No privilege escalation or gateway creation: operator must inject a qualified
service transport and a qualified gateway runtime separately.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.supervised.live_supervisor import (
    submit_live_reviewer_once,
    submit_live_worker_once,
)

if TYPE_CHECKING:
    from hermes_harness.sandbox.live_sandbox_plan import SandboxPlan
    from hermes_harness.services.live_service_controller import ServiceController
    from hermes_harness.supervised.live_execution_binding import (
        ExecutionBinding,
        ExecutionBindingStore,
    )
    from hermes_harness.supervised.live_supervisor import LiveSupervisorInputs


class LiveCoordinatorError(RuntimeError):
    """Unsafe role progression or mismatched service binding."""


@dataclass(frozen=True, slots=True)
class LiveExecutionCoordinator:
    """Fail-closed role admission with durable service identity."""

    bindings: ExecutionBindingStore
    services: ServiceController

    def reserve_and_launch(self, binding: ExecutionBinding, plan: SandboxPlan) -> None:
        """Persist binding before launch and reject role or unit mismatches."""
        if plan.unit != binding.unit or plan.role != binding.role:
            msg = 'Plan does not match task/role binding'
            raise LiveCoordinatorError(msg)
        self.bindings.bind(binding, self.services.journal)
        self.services.launch_once(plan)

    async def submit_worker(self, binding: ExecutionBinding, inputs: LiveSupervisorInputs) -> None:
        """Dispatch only into a matching worker service already observed running."""
        if binding.role != 'worker':
            msg = 'Worker role required'
            raise LiveCoordinatorError(msg)
        if binding.task_id != inputs.plan.task_id:
            msg = 'Task mismatch'
            raise LiveCoordinatorError(msg)
        if binding.submission_id != inputs.worker_submission.submission_id:
            msg = 'Worker submission mismatch'
            raise LiveCoordinatorError(msg)
        if binding.session_id != inputs.worker_submission.session_id:
            msg = 'Worker session mismatch'
            raise LiveCoordinatorError(msg)
        self._require_current_service(binding)
        await submit_live_worker_once(inputs)

    async def submit_reviewer(
        self, binding: ExecutionBinding, inputs: LiveSupervisorInputs
    ) -> None:
        """Dispatch only after existing handoff and reviewer gates validate."""
        if binding.role != 'reviewer':
            msg = 'Reviewer role required'
            raise LiveCoordinatorError(msg)
        if binding.task_id != inputs.plan.task_id:
            msg = 'Task mismatch'
            raise LiveCoordinatorError(msg)
        if binding.submission_id != inputs.reviewer_submission.submission_id:
            msg = 'Reviewer submission mismatch'
            raise LiveCoordinatorError(msg)
        if binding.session_id != inputs.reviewer_submission.session_id:
            msg = 'Reviewer session mismatch'
            raise LiveCoordinatorError(msg)
        self._require_current_service(binding)
        await submit_live_reviewer_once(inputs)

    def _require_current_service(self, binding: ExecutionBinding) -> None:
        """Require both durable identity and a fresh systemd observation.

        Historical RUNNING_OBSERVED records are not proof of current execution.
        Any status transport error propagates and blocks gateway admission.
        """
        self.bindings.require_running(binding, self.services.journal)
        if not self.services.transport.is_active(binding.unit):
            msg = 'Bound service is no longer authoritatively active'
            raise LiveCoordinatorError(msg)

    def inspect_bound_service(self, binding: ExecutionBinding) -> bool:
        """Read-only live status; never submits or mutates either ledger."""
        self.bindings.require_running(binding, self.services.journal)
        return self.services.transport.is_active(binding.unit)
