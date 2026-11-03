"""No live inference or subprocesses: deterministic control-plane contract tests."""

import json
from pathlib import Path

import pytest

from hermes_harness.core.state import Stage, create, save
from hermes_harness.gateway.hermes_adapter import TaskKind
from hermes_harness.supervised.orchestration import DispatchError, TaskSpec, dispatch_offline


class FixedWorker:
    def run(self, _invocation: object) -> str:
        return 'Verified offline response'


class FailingWorker:
    def run(self, _invocation: object) -> str:
        msg = 'Simulated failure'
        raise RuntimeError(msg)


def _spec(tmp_path: Path, kind: TaskKind = TaskKind.SCOUT) -> TaskSpec:
    return TaskSpec(ticket_id='offline-001', kind=kind, prompt='Review tests', workspace=tmp_path)


def test_scout_creates_private_evidence(tmp_path: Path) -> None:
    create(tmp_path, 'offline-001', 'Test')
    result = dispatch_offline(tmp_path, _spec(tmp_path), FixedWorker())
    output = Path(result.output_path)
    assert output.read_text() == 'Verified offline response'
    expected_mode = 0o600
    assert output.stat().st_mode & 0o777 == expected_mode
    record = json.loads((output.parent / 'result.json').read_text())
    assert record['model'] == 'granite4:3b'
    assert record['status'] == 'succeeded'


def test_stage_mismatch_refuses_dispatch(tmp_path: Path) -> None:
    create(tmp_path, 'offline-001', 'Test')
    with pytest.raises(DispatchError, match='not allowed'):
        dispatch_offline(tmp_path, _spec(tmp_path, TaskKind.REVIEW), FixedWorker())


def test_cyber_rejected_before_any_worker_call(tmp_path: Path) -> None:
    create(tmp_path, 'offline-001', 'Test')
    with pytest.raises(DispatchError, match='not authorized'):
        dispatch_offline(tmp_path, _spec(tmp_path, TaskKind.CYBER), FixedWorker())


def test_worker_failure_persists_sanitized_record(tmp_path: Path) -> None:
    create(tmp_path, 'offline-001', 'Test')
    with pytest.raises(RuntimeError, match='Simulated'):
        dispatch_offline(tmp_path, _spec(tmp_path), FailingWorker())
    records = list((tmp_path / 'tickets' / 'offline-001' / 'runs').glob('*/failure.json'))
    assert len(records) == 1
    failure = json.loads(records[0].read_text())
    assert failure['error_type'] == 'RuntimeError'
    assert 'Simulated failure' not in records[0].read_text()


def test_plan_routed_to_codex_at_ticket_stage(tmp_path: Path) -> None:
    ticket = create(tmp_path, 'offline-001', 'Test')
    ticket.stage = Stage.TICKET
    save(tmp_path, ticket)
    result = dispatch_offline(tmp_path, _spec(tmp_path, TaskKind.PLAN), FixedWorker())
    assert (result.provider, result.model) == ('openai-codex', 'gpt-6.1-sol')
