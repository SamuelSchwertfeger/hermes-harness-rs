"""Safety-focused pipeline unit tests (zero gateway or provider calls)."""

from unittest.mock import Mock

import pytest

from hermes_harness.supervised.pipeline import (
    PipelineBlockedError,
    PipelineInputs,
    QualifiedPipeline,
)


def _pipeline() -> QualifiedPipeline:
    identity = Mock(task_id='task', worker_submission_id='w1', reviewer_submission_id='r1')
    binding = Mock(
        task_id='task', role='worker', unit='hermes-job-w', submission_id='w1', session_id='sw'
    )
    reviewer = Mock(
        task_id='task', role='reviewer', unit='hermes-job-r', submission_id='r1', session_id='sr'
    )
    supervisor = Mock()
    supervisor.worker_submission.session_id = 'sw'
    supervisor.worker_submission.submission_id = 'w1'
    supervisor.reviewer_submission.session_id = 'sr'
    supervisor.reviewer_submission.submission_id = 'r1'
    recovery = Mock()
    recovery.initialize.return_value.phase = 'planned'
    inputs = PipelineInputs(
        coordinator=Mock(),
        supervisor=supervisor,
        worker_binding=binding,
        reviewer_binding=reviewer,
        worker_plan=Mock(role='worker', unit='hermes-job-w'),
        reviewer_plan=Mock(role='reviewer', unit='hermes-job-r'),
        recovery=recovery,
        identity=identity,
        output_dir=Mock(),
    )
    return QualifiedPipeline(inputs)


def test_worker_launch_requires_separate_authorization() -> None:
    pipeline = _pipeline()
    with pytest.raises(PipelineBlockedError, match='authorization'):
        pipeline.launch_worker()
    pipeline.inputs.coordinator.reserve_and_launch.assert_not_called()


def test_role_identity_mismatch_blocks_even_when_authorized() -> None:
    pipeline = _pipeline()
    pipeline.inputs.reviewer_binding.unit = 'hermes-job-w'
    with pytest.raises(PipelineBlockedError, match='distinct'):
        pipeline.launch_worker(authorized=True)
    pipeline.inputs.coordinator.reserve_and_launch.assert_not_called()


def test_authorized_launch_is_single_explicit_action() -> None:
    pipeline = _pipeline()
    pipeline.launch_worker(authorized=True)
    pipeline.inputs.coordinator.reserve_and_launch.assert_called_once_with(
        pipeline.inputs.worker_binding, pipeline.inputs.worker_plan
    )
    pipeline.inputs.coordinator.submit_worker.assert_not_called()
    pipeline.inputs.coordinator.submit_reviewer.assert_not_called()
