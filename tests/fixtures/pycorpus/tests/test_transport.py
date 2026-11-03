"""Real-process transport tests; no model calls or network traffic."""

import json
import sys
from pathlib import Path
from threading import Event

import pytest

from hermes_harness.core.state import create
from hermes_harness.core.transport import (
    ForegroundFixtureWorker,
    ForegroundOptions,
    TransportError,
)
from hermes_harness.core.watchdog import Budget
from hermes_harness.gateway.hermes_adapter import TaskKind
from hermes_harness.supervised.orchestration import TaskSpec, dispatch_offline


def _fixture(tmp_path: Path, code: str, *, deadline: float = 3.0) -> ForegroundFixtureWorker:
    return ForegroundFixtureWorker(
        ForegroundOptions(
            argv=(sys.executable, '-c', code),
            records=tmp_path / 'transport',
            budget=Budget(deadline_seconds=deadline, grace_seconds=0.2),
        )
    )


def _dispatch(tmp_path: Path, worker: ForegroundFixtureWorker) -> str:
    create(tmp_path, 'fixture-001', 'Fixture transport')
    result = dispatch_offline(
        tmp_path,
        TaskSpec(
            ticket_id='fixture-001',
            kind=TaskKind.SCOUT,
            prompt='Summarize offline fixture',
            workspace=tmp_path,
        ),
        worker,
    )
    return Path(result.output_path).read_text()


def test_request_handoff_and_audit(tmp_path: Path) -> None:
    code = 'import json,sys; print(json.load(open(sys.argv[-1]))["kind"])'
    assert _dispatch(tmp_path, _fixture(tmp_path, code)).strip() == 'scout'
    request = next((tmp_path / 'transport').glob('fixture-*/request.json'))
    expected_mode = 0o600
    assert request.stat().st_mode & 0o777 == expected_mode
    assert json.loads(request.read_text())['execution_enabled'] is False
    records = list((tmp_path / 'transport').glob('fixture-*/*/run.json'))
    assert len(records) == 1
    assert json.loads(records[0].read_text())['outcome'] == 'completed'


def test_nonzero_failure_is_not_success(tmp_path: Path) -> None:
    worker = _fixture(tmp_path, 'import sys; sys.exit(3)')
    create(tmp_path, 'fixture-001', 'Fixture transport')
    spec = TaskSpec(
        ticket_id='fixture-001', kind=TaskKind.SCOUT, prompt='Review fixture', workspace=tmp_path
    )
    with pytest.raises(TransportError, match='without success'):
        dispatch_offline(tmp_path, spec, worker)


def test_timeout_is_rejected(tmp_path: Path) -> None:
    worker = _fixture(tmp_path, 'import time; time.sleep(20)', deadline=0.1)
    create(tmp_path, 'fixture-001', 'Fixture transport')
    spec = TaskSpec(
        ticket_id='fixture-001', kind=TaskKind.SCOUT, prompt='Review fixture', workspace=tmp_path
    )
    with pytest.raises(TransportError, match='timed_out'):
        dispatch_offline(tmp_path, spec, worker)


def test_cancelled_before_dispatch(tmp_path: Path) -> None:
    event = Event()
    event.set()
    worker = ForegroundFixtureWorker(
        ForegroundOptions(
            argv=(sys.executable, '-c', 'import time; time.sleep(20)'),
            records=tmp_path / 'transport',
        ),
        cancel=event,
    )
    create(tmp_path, 'fixture-001', 'Fixture transport')
    spec = TaskSpec(
        ticket_id='fixture-001', kind=TaskKind.SCOUT, prompt='Review fixture', workspace=tmp_path
    )
    with pytest.raises(TransportError, match='cancelled'):
        dispatch_offline(tmp_path, spec, worker)


def test_reject_live_binary(tmp_path: Path) -> None:
    path = tmp_path / 'hermes'
    path.write_text('fake')
    with pytest.raises(PermissionError, match='Live model'):
        ForegroundOptions(argv=(str(path),), records=tmp_path).validate()
