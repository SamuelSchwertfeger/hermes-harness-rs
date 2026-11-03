"""Negative and recovery tests for append-only native repair admission."""

import hashlib
import json
import stat
from pathlib import Path

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.repair_controller import (
    inspect_repair,
    open_repair,
    record_repair_disposition,
    reserve_repair,
)

EXPECTED_EVIDENCE_MODE = 0o600
EXPECTED_REMAINING = 2
EXPECTED_ATTEMPTS = 3


def _receipt(path: Path, record: dict[str, object]) -> str:
    payload = (json.dumps(record, sort_keys=True) + '\n').encode()
    path.write_bytes(payload)
    return hashlib.sha256(payload).hexdigest()


def _fixture(tmp_path: Path) -> tuple[Path, Path, Path]:
    state = tmp_path / 'state'
    state.mkdir()
    checkpoint = tmp_path / 'checkpoint.json'
    digest = _receipt(
        checkpoint,
        {
            'mode': 'integrity-checkpoint',
            'task_id': 'task-1',
            'baseline': 'a' * 40,
            'workspace': str(tmp_path / 'worktree'),
            'max_repair_attempts': 3,
        },
    )
    failure = tmp_path / 'failure.json'
    _receipt(
        failure,
        {
            'mode': 'sandboxed-functional-verification',
            'task_id': 'task-1',
            'baseline': 'a' * 40,
            'workspace': str(tmp_path / 'worktree'),
            'checkpoint_sha256': digest,
            'functional_verified': False,
            'tests_executed': True,
            'timed_out': False,
            'sandbox_started': True,
            'human_decision_pending': True,
        },
    )
    return state, checkpoint, failure


def test_open_and_reserve_append_only(tmp_path: Path) -> None:
    state, checkpoint, failure = _fixture(tmp_path)
    opened = open_repair(failure=failure, checkpoint=checkpoint, state=state, authorized=True)
    assert opened['automatic_execution_authorized'] is False
    assert stat.S_IMODE(Path(str(opened['evidence'])).stat().st_mode) == EXPECTED_EVIDENCE_MODE
    reserved = reserve_repair(state=state, task_id='task-1', authorized=True)
    assert reserved['number'] == 1
    assert reserved['generated_code_executed'] is False
    assert inspect_repair(state=state, task_id='task-1')['remaining'] == EXPECTED_REMAINING
    with pytest.raises(WorkflowError, match='Prior attempt pending'):
        reserve_repair(state=state, task_id='task-1', authorized=True)
    with pytest.raises(FileExistsError):
        open_repair(failure=failure, checkpoint=checkpoint, state=state, authorized=True)


def test_rejects_missing_authorization(tmp_path: Path) -> None:
    state, checkpoint, failure = _fixture(tmp_path)
    with pytest.raises(WorkflowError, match='authorization'):
        open_repair(failure=failure, checkpoint=checkpoint, state=state)
    open_repair(failure=failure, checkpoint=checkpoint, state=state, authorized=True)
    with pytest.raises(WorkflowError, match='authorization'):
        reserve_repair(state=state, task_id='task-1')


@pytest.mark.parametrize(
    ('field', 'value'),
    [
        ('tests_executed', False),
        ('sandbox_started', False),
        ('timed_out', True),
        ('functional_verified', True),
        ('checkpoint_sha256', '0' * 64),
    ],
)
def test_refuses_unqualified_failures(tmp_path: Path, field: str, value: object) -> None:
    state, checkpoint, failure = _fixture(tmp_path)
    record = json.loads(failure.read_text())
    record[field] = value
    _receipt(failure, record)
    with pytest.raises(WorkflowError):
        open_repair(failure=failure, checkpoint=checkpoint, state=state, authorized=True)
    assert not list(state.iterdir())


def test_refuses_modified_reservation(tmp_path: Path) -> None:
    state, checkpoint, failure = _fixture(tmp_path)
    open_repair(failure=failure, checkpoint=checkpoint, state=state, authorized=True)
    reserve_repair(state=state, task_id='task-1', authorized=True)
    path = state / 'task-1-repair-1.json'
    record = json.loads(path.read_text())
    record['number'] = 2
    _receipt(path, record)
    with pytest.raises(WorkflowError, match='Reservation mismatch'):
        inspect_repair(state=state, task_id='task-1')


def _started(tmp_path: Path) -> tuple[Path, Path, dict[str, object]]:
    state, checkpoint, failure = _fixture(tmp_path)
    open_repair(failure=failure, checkpoint=checkpoint, state=state, authorized=True)
    reservation = reserve_repair(state=state, task_id='task-1', authorized=True)
    return state, checkpoint, reservation


def test_abandoned_attempt_closes_sequence(tmp_path: Path) -> None:
    state, _checkpoint_file, _ = _started(tmp_path)
    result = record_repair_disposition(
        state=state, task_id='task-1', outcome='abandoned', authorized=True
    )
    assert result['next_stage'] == 'finished'
    assert inspect_repair(state=state, task_id='task-1')['terminal'] is True
    with pytest.raises(WorkflowError, match='pending or sequence closed'):
        reserve_repair(state=state, task_id='task-1', authorized=True)
    with pytest.raises(WorkflowError, match='No pending attempt'):
        record_repair_disposition(
            state=state, task_id='task-1', outcome='abandoned', authorized=True
        )


def test_rejected_attempt_requires_review(tmp_path: Path) -> None:
    state, _checkpoint_file, _ = _started(tmp_path)
    with pytest.raises(WorkflowError, match='Independent review'):
        record_repair_disposition(
            state=state, task_id='task-1', outcome='rejected', authorized=True
        )


def test_three_rejections_exhaust_budget(tmp_path: Path) -> None:
    state, _checkpoint, reservation = _started(tmp_path)
    opened = json.loads((state / 'task-1-repair-open.json').read_text())
    for number in range(1, EXPECTED_ATTEMPTS + 1):
        claim: dict[str, object] = {
            'mode': 'repair-dispatch-claim',
            'task_id': 'task-1',
            'number': number,
            'reservation_sha256': reservation['evidence_sha256'],
            'worker_model': 'worker',
            'reviewer_model': 'reviewer',
            'owned_paths': ['existing.py'],
            'preimages': {'existing.py': 'b' * 64},
        }
        claim_hash = _receipt(state / f'task-1-repair-{number}-dispatch.json', claim)
        path = state / f'task-1-repair-{number}-review.json'
        _receipt(
            path,
            {
                'mode': 'repair-independent-review',
                'task_id': 'task-1',
                'number': number,
                'reservation_sha256': reservation['evidence_sha256'],
                'claim_sha256': claim_hash,
                'checkpoint_sha256': opened['checkpoint_sha256'],
                'source_failure_sha256': opened['failure_sha256'],
                'baseline': opened['baseline'],
                'workspace': opened['workspace'],
                'owned_paths': ['existing.py'],
                'worker_model': 'worker',
                'reviewer_model': 'reviewer',
                'verdict': 'changes_required',
                'replacements': [
                    {
                        'path': 'existing.py',
                        'expected_sha256': 'b' * 64,
                        'content': 'x = 1\n',
                        'content_sha256': hashlib.sha256(b'x = 1\n').hexdigest(),
                    }
                ],
            },
        )
        record_repair_disposition(
            state=state, task_id='task-1', outcome='rejected', review=path, authorized=True
        )
        if number < EXPECTED_ATTEMPTS:
            reservation = reserve_repair(state=state, task_id='task-1', authorized=True)
    result = inspect_repair(state=state, task_id='task-1')
    assert result['attempts_used'] == EXPECTED_ATTEMPTS
    assert result['next_stage'] == 'exhausted'
    with pytest.raises(WorkflowError):
        reserve_repair(state=state, task_id='task-1', authorized=True)


def test_attempt_disposition_requires_authorization(tmp_path: Path) -> None:
    state, _checkpoint_file, _ = _started(tmp_path)
    with pytest.raises(WorkflowError, match='authorization'):
        record_repair_disposition(state=state, task_id='task-1', outcome='abandoned')
    assert (
        inspect_repair(state=state, task_id='task-1')['next_stage'] == 'await-attempt-disposition'
    )
