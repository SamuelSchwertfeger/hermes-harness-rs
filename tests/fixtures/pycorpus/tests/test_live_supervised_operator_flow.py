"""Provider-free checks for explicit operator role boundaries."""

from dataclasses import dataclass, fields
from unittest.mock import Mock, patch

import pytest

from hermes_harness.operator.live_operator_safety import OperatorSafetyError
from hermes_harness.supervised.live_supervised_operator_flow import (
    OperatorFlowError,
    OperatorFlowInputs,
    SupervisedOperatorFlow,
    _verify_sandbox_mounts,
)
from hermes_harness.supervised.live_supervised_recovery import Phase
from test_live_operator_safety import REVIEWER, WORKER


@dataclass(frozen=True)
class FakePlan:
    systemd_argv: tuple[str, ...]


def test_reviewer_writable_bind_denied() -> None:
    inputs = Mock(spec=[field.name for field in fields(OperatorFlowInputs)])
    inputs.worker_plan = FakePlan(WORKER)
    inputs.reviewer_plan = FakePlan((
        *REVIEWER[:-3],
        '--bind',
        '/safe/extra',
        '/extra',
        *REVIEWER[-3:],
    ))
    with pytest.raises(OperatorSafetyError, match='read-only'):
        _verify_sandbox_mounts(inputs)


def test_reviewer_without_network_isolation_denied() -> None:
    inputs = Mock(spec=[field.name for field in fields(OperatorFlowInputs)])
    inputs.worker_plan = FakePlan(WORKER)
    inputs.reviewer_plan = FakePlan(tuple(x for x in REVIEWER if x != '--unshare-net'))
    with pytest.raises(OperatorSafetyError, match='isolation'):
        _verify_sandbox_mounts(inputs)


def test_no_implicit_authorization() -> None:
    flow = SupervisedOperatorFlow(Mock(spec=[field.name for field in fields(OperatorFlowInputs)]))
    with pytest.raises(OperatorFlowError, match='authorization'):
        flow.launch_worker()
    with pytest.raises(OperatorFlowError, match='authorization'):
        flow.launch_reviewer()


def test_wrong_phase_blocks_dispatch() -> None:
    flow = SupervisedOperatorFlow(Mock(spec=[field.name for field in fields(OperatorFlowInputs)]))
    with patch.object(SupervisedOperatorFlow, '_checkpoint', return_value=Mock(phase=Phase.HALTED)):
        with pytest.raises(OperatorFlowError, match='reconcile'):
            flow.launch_worker(authorized=True)
        with pytest.raises(OperatorFlowError, match='reconcile'):
            flow.launch_reviewer(authorized=True)


def test_worker_launch_requires_explicit_authorization_before_inspection() -> None:
    inputs = Mock(spec=[field.name for field in fields(OperatorFlowInputs)])
    flow = SupervisedOperatorFlow(inputs)
    with pytest.raises(OperatorFlowError, match='authorization'):
        flow.launch_worker()
    inputs.coordinator.reserve_and_launch.assert_not_called()


def test_reviewer_launch_does_not_submit_gateway_work() -> None:
    inputs = Mock(spec=[field.name for field in fields(OperatorFlowInputs)])
    flow = SupervisedOperatorFlow(inputs)
    with patch.object(
        SupervisedOperatorFlow, '_checkpoint', return_value=Mock(phase=Phase.WORKER_AUDITED)
    ):
        flow.launch_reviewer(authorized=True)
    inputs.coordinator.reserve_and_launch.assert_called_once_with(
        inputs.reviewer_binding, inputs.reviewer_plan
    )
    inputs.coordinator.submit_reviewer.assert_not_called()


def test_worker_launch_does_not_submit_gateway_work() -> None:
    inputs = Mock(spec=[field.name for field in fields(OperatorFlowInputs)])
    flow = SupervisedOperatorFlow(inputs)
    with patch.object(
        SupervisedOperatorFlow, '_checkpoint', return_value=Mock(phase=Phase.PLANNED)
    ):
        flow.launch_worker(authorized=True)
    inputs.coordinator.reserve_and_launch.assert_called_once_with(
        inputs.worker_binding, inputs.worker_plan
    )
    inputs.coordinator.submit_worker.assert_not_called()
