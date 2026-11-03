"""One-shot bounded repair dispatch, interrupted recovery and trust boundaries."""

import hashlib
import json
from pathlib import Path
from unittest.mock import Mock

import pytest

from hermes_harness.multiagent.local_inference import LocalEndpoint
from hermes_harness.multiagent.replace_edits import FileReplacement
from hermes_harness.multiagent.replacement_pipeline import ReplacementReview
from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native import repair_dispatch
from hermes_harness.native.repair_controller import (
    open_repair,
    record_repair_disposition,
    reserve_repair,
)


def _write(path: Path, value: dict[str, object]) -> str:
    payload = (json.dumps(value, sort_keys=True) + '\n').encode()
    path.write_bytes(payload)
    return hashlib.sha256(payload).hexdigest()


def _setup(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> tuple[Path, Path, Mock]:
    state = tmp_path / 'state'
    state.mkdir()
    workspace = tmp_path / 'worktree'
    workspace.mkdir()
    source = b'print(0)\n'
    (workspace / 'existing.py').write_bytes(source)
    digest_before = hashlib.sha256(source).hexdigest()
    checkpoint = tmp_path / 'checkpoint.json'
    original: dict[str, object] = {
        'mode': 'integrity-checkpoint',
        'task_id': 'task-1',
        'baseline': 'a' * 40,
        'workspace': str(workspace),
        'max_repair_attempts': 3,
        'files': [
            {'path': 'existing.py', 'sha256': digest_before},
        ],
    }
    digest = _write(checkpoint, original)
    failure = tmp_path / 'failed.json'
    _write(
        failure,
        {
            'mode': 'sandboxed-functional-verification',
            'task_id': 'task-1',
            'baseline': 'a' * 40,
            'workspace': str(workspace),
            'checkpoint_sha256': digest,
            'functional_verified': False,
            'tests_executed': True,
            'sandbox_started': True,
            'timed_out': False,
            'human_decision_pending': True,
        },
    )
    open_repair(failure=failure, checkpoint=checkpoint, state=state, authorized=True)
    reserve_repair(state=state, task_id='task-1', authorized=True)
    monkeypatch.setattr(
        repair_dispatch, '_audit_checkout', lambda _w, _c: {'existing.py': digest_before}
    )
    generate = Mock(
        return_value=ReplacementReview(
            'task-1',
            (FileReplacement('existing.py', digest_before, 'print(1)\n'),),
            'qwen3:4b',
            'accept',
            (),
        )
    )
    monkeypatch.setattr(repair_dispatch, 'generate_reviewed_replacements', generate)
    return state, checkpoint, generate


def _dispatch(state: Path, checkpoint: Path, **kwargs: object) -> dict[str, object]:
    return repair_dispatch.dispatch_repair(
        state=state,
        task_id='task-1',
        checkpoint=checkpoint,
        objective='Fix failed test',
        owned_paths=('existing.py',),
        authorized=True,
        **kwargs,
    )


def test_accept_and_replay_fence(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    state, checkpoint, generate = _setup(tmp_path, monkeypatch)
    result = _dispatch(state, checkpoint)
    assert result['verdict'] == 'accept'
    assert result['applied'] is False
    assert result['generated_code_executed'] is False
    assert (
        repair_dispatch.inspect_dispatch(state=state, task_id='task-1')['status']
        == 'review-recorded'
    )
    with pytest.raises(WorkflowError, match='already dispatched'):
        _dispatch(state, checkpoint)
    assert generate.call_count == 1


def test_rejected_review_operator_disposition(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    state, checkpoint, generate = _setup(tmp_path, monkeypatch)
    digest = hashlib.sha256(b'print(0)\n').hexdigest()
    generate.return_value = ReplacementReview(
        'task-1',
        (FileReplacement('existing.py', digest, 'x = 1\n'),),
        'qwen3:4b',
        'changes_required',
        ('Incorrect logic',),
    )
    result = _dispatch(state, checkpoint)
    recorded = record_repair_disposition(
        state=state,
        task_id='task-1',
        outcome='rejected',
        review=Path(str(result['evidence'])),
        authorized=True,
    )
    assert recorded['outcome'] == 'rejected'


def test_interrupted_dispatch_does_not_repeat(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    state, checkpoint, generate = _setup(tmp_path, monkeypatch)
    generate.side_effect = RuntimeError('model transport interrupted')
    with pytest.raises(RuntimeError, match='interrupted'):
        _dispatch(state, checkpoint)
    assert (
        repair_dispatch.inspect_dispatch(state=state, task_id='task-1')['status']
        == 'dispatched-unresolved'
    )
    with pytest.raises(WorkflowError, match='already dispatched'):
        _dispatch(state, checkpoint)
    assert generate.call_count == 1


@pytest.mark.parametrize(
    'case', ['unowned', 'wrong-model', 'bad-preimage', 'unauthorized', 'stale']
)
def test_dispatch_refuses_invalid_inputs(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    case: str,
) -> None:
    state, checkpoint, generate = _setup(tmp_path, monkeypatch)
    if case == 'unowned':
        with pytest.raises(WorkflowError):
            repair_dispatch.dispatch_repair(
                state=state,
                task_id='task-1',
                checkpoint=checkpoint,
                objective='Repair',
                owned_paths=('not-owned.py',),
                authorized=True,
            )
    elif case == 'wrong-model':
        endpoint = LocalEndpoint('http://127.0.0.1:11434/v1/chat/completions', 'same')
        with pytest.raises(WorkflowError, match='differ'):
            _dispatch(state, checkpoint, worker=endpoint, reviewer=endpoint)
    elif case == 'bad-preimage':
        generate.return_value = ReplacementReview(
            'task-1',
            (FileReplacement('existing.py', 'c' * 64, 'x\n'),),
            'qwen3:4b',
            'accept',
            (),
        )
        with pytest.raises(WorkflowError, match='preimage'):
            _dispatch(state, checkpoint)
        assert (
            repair_dispatch.inspect_dispatch(state=state, task_id='task-1')['status']
            == 'dispatched-unresolved'
        )
    elif case == 'unauthorized':
        with pytest.raises(WorkflowError, match='authorization'):
            repair_dispatch.dispatch_repair(
                state=state,
                task_id='task-1',
                checkpoint=checkpoint,
                objective='Repair',
                owned_paths=('existing.py',),
            )
    else:
        monkeypatch.setattr(
            repair_dispatch, '_audit_checkout', lambda _w, _c: {'existing.py': 'c' * 64}
        )
        with pytest.raises(WorkflowError, match='drift'):
            _dispatch(state, checkpoint)
    assert generate.call_count == (1 if case == 'bad-preimage' else 0)


def test_fake_review_cannot_settle_attempt(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    state, checkpoint, _generate = _setup(tmp_path, monkeypatch)
    fake = state / 'fake.json'
    _write(
        fake,
        {
            'mode': 'repair-independent-review',
            'task_id': 'task-1',
            'number': 1,
            'verdict': 'changes_required',
            'worker_model': 'worker',
            'reviewer_model': 'reviewer',
        },
    )
    with pytest.raises(WorkflowError, match='native repair dispatch'):
        record_repair_disposition(
            state=state,
            task_id='task-1',
            outcome='rejected',
            review=fake,
            authorized=True,
        )
    assert (
        repair_dispatch.inspect_dispatch(state=state, task_id='task-1')['status']
        == 'not-dispatched'
    )
    _dispatch(state, checkpoint)


def test_no_op_model_acceptance_is_policy_rejected(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    state, checkpoint, generate = _setup(tmp_path, monkeypatch)
    preimage = hashlib.sha256(b'print(0)\n').hexdigest()
    generate.return_value = ReplacementReview(
        'task-1',
        (FileReplacement('existing.py', preimage, 'print(0)\n'),),
        'qwen3:4b',
        'accept',
        ('Looks correct',),
    )
    result = _dispatch(state, checkpoint)
    assert result['model_verdict'] == 'accept'
    assert result['verdict'] == 'changes_required'
    assert result['policy_rejected_no_op'] is True
    assert any('No-op replacement' in finding for finding in result['findings'])
    assert result['applied'] is False
    assert generate.call_count == 1
    recorded = record_repair_disposition(
        state=state,
        task_id='task-1',
        outcome='rejected',
        review=Path(str(result['evidence'])),
        authorized=True,
    )
    assert recorded['outcome'] == 'rejected'


def test_changed_source_not_rejected_as_no_op(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    state, checkpoint, _generate = _setup(tmp_path, monkeypatch)
    result = _dispatch(state, checkpoint)
    assert result['verdict'] == 'accept'
    assert result['model_verdict'] == 'accept'
    assert result['policy_rejected_no_op'] is False
    assert result['policy_rejected_invalid_syntax'] is False


def test_replay_preserves_evidence_hashes(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    state, checkpoint, generate = _setup(tmp_path, monkeypatch)
    _dispatch(state, checkpoint)
    paths = [state / f'task-1-repair-1-{suffix}.json' for suffix in ('dispatch', 'review')]
    before = [hashlib.sha256(path.read_bytes()).hexdigest() for path in paths]
    with pytest.raises(WorkflowError, match='already dispatched'):
        _dispatch(state, checkpoint)
    assert [hashlib.sha256(path.read_bytes()).hexdigest() for path in paths] == before
    assert generate.call_count == 1


def test_invalid_python_acceptance_is_policy_rejected(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    state, checkpoint, generate = _setup(tmp_path, monkeypatch)
    preimage = hashlib.sha256(b'print(0)\n').hexdigest()
    generate.return_value = ReplacementReview(
        'task-1',
        (FileReplacement('existing.py', preimage, 'print(1)\\n'),),
        'qwen3:4b',
        'accept',
        ('Looks correct',),
    )
    result = _dispatch(state, checkpoint)
    assert result['model_verdict'] == 'accept'
    assert result['verdict'] == 'changes_required'
    assert result['policy_rejected_invalid_syntax'] is True
    assert any('Invalid Python syntax' in finding for finding in result['findings'])
    assert generate.call_count == 1
