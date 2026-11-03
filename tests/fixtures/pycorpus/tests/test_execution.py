import sys
from pathlib import Path  # noqa: TC003 - runtime annotation on Python 3.13

from hermes_harness.core.execution import run_bounded


def test_success(tmp_path: Path) -> None:
    result = run_bounded([sys.executable, '-c', "print('ok')"], cwd=tmp_path)
    assert result.returncode == 0
    assert result.stdout.strip() == 'ok'


def test_timeout(tmp_path: Path) -> None:
    result = run_bounded(
        [sys.executable, '-c', 'import time; time.sleep(30)'], cwd=tmp_path, timeout=0.1
    )
    assert result.timed_out
