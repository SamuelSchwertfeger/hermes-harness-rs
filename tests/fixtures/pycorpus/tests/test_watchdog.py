"""Offline watchdog lifecycle regression tests."""

import json
import sys
from pathlib import Path
from threading import Event

from hermes_harness.core.watchdog import Budget, SupervisionOptions, supervise


def test_completion(tmp_path: Path) -> None:
    result = supervise(
        [sys.executable, '-c', 'print(42)'],
        options=SupervisionOptions(cwd=tmp_path, records=tmp_path),
    )
    assert result.outcome == 'completed'
    assert result.returncode == 0
    assert not result.group_alive
    assert Path(result.stdout_path).read_text().strip() == '42'
    record = json.loads((tmp_path / result.run_id / 'run.json').read_text())
    assert record['run_id'] == result.run_id


def test_deadline(tmp_path: Path) -> None:
    budget = Budget(deadline_seconds=0.1, grace_seconds=0.3, poll_seconds=0.01)
    result = supervise(
        [sys.executable, '-c', 'import time; time.sleep(10)'],
        options=SupervisionOptions(cwd=tmp_path, records=tmp_path, budget=budget),
    )
    assert result.outcome == 'timed_out'
    assert not result.group_alive


def test_cancel(tmp_path: Path) -> None:
    cancel = Event()
    cancel.set()
    result = supervise(
        [sys.executable, '-c', 'import time; time.sleep(10)'],
        options=SupervisionOptions(cwd=tmp_path, records=tmp_path),
        cancel=cancel,
    )
    assert result.outcome == 'cancelled'
    assert not result.group_alive


def test_bad_budget(tmp_path: Path) -> None:
    try:
        supervise(
            ['echo', 'bad'],
            options=SupervisionOptions(cwd=tmp_path, records=tmp_path, budget=Budget(0)),
        )
    except ValueError:
        return
    msg = 'Expected invalid budget to be rejected'
    raise AssertionError(msg)
