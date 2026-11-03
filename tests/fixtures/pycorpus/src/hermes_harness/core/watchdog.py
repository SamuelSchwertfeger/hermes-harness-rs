"""Supervise owned foreground process groups; never supervise detached Hermes gateway jobs."""

import json
import logging
import os
import signal
import subprocess
import time
import uuid
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from pathlib import Path
from threading import Event
from typing import Literal

LOGGER = logging.getLogger(__name__)
type Outcome = Literal['completed', 'timed_out', 'cancelled', 'cleanup_failed']


@dataclass(frozen=True, slots=True)
class Budget:
    deadline_seconds: float = 30.0
    grace_seconds: float = 1.0
    poll_seconds: float = 0.05

    def validate(self) -> None:
        if min(self.deadline_seconds, self.grace_seconds, self.poll_seconds) <= 0:
            msg = 'All watchdog timing budgets must be positive'
            raise ValueError(msg)


@dataclass(frozen=True, slots=True)
class SupervisionOptions:
    cwd: Path
    records: Path
    budget: Budget = Budget()


@dataclass(frozen=True, slots=True)
class RunRecord:
    run_id: str
    argv: tuple[str, ...]
    pid: int
    pgid: int
    started_at: str
    finished_at: str
    outcome: Outcome
    returncode: int | None
    group_alive: bool
    stdout_path: str
    stderr_path: str


def _group_alive(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _signal_group(pgid: int, sig: signal.Signals) -> None:
    try:
        os.killpg(pgid, sig)
    except ProcessLookupError:
        return


def _quiesce(child: subprocess.Popen[bytes], budget: Budget) -> bool:
    """Terminate only the owned process group, with bounded escalation."""
    _signal_group(child.pid, signal.SIGTERM)
    end = time.monotonic() + budget.grace_seconds
    while time.monotonic() < end and _group_alive(child.pid):
        time.sleep(budget.poll_seconds)
    if _group_alive(child.pid):
        _signal_group(child.pid, signal.SIGKILL)
    end = time.monotonic() + budget.grace_seconds
    while time.monotonic() < end and _group_alive(child.pid):
        time.sleep(budget.poll_seconds)
    try:
        child.wait(timeout=budget.grace_seconds)
    except subprocess.TimeoutExpired:
        return False
    return not _group_alive(child.pid)


def _record(directory: Path, result: RunRecord) -> None:
    target = directory / 'run.json'
    temporary = directory / 'run.json.tmp'
    temporary.write_text(json.dumps(asdict(result), indent=2) + '\n', encoding='utf-8')
    temporary.replace(target)


def _monitor(child: subprocess.Popen[bytes], budget: Budget, cancel: Event | None) -> Outcome:
    cutoff = time.monotonic() + budget.deadline_seconds
    outcome: Outcome = 'completed'
    while child.poll() is None:
        if cancel is not None and cancel.is_set():
            outcome = 'cancelled'
            break
        if time.monotonic() >= cutoff:
            outcome = 'timed_out'
            break
        time.sleep(budget.poll_seconds)
    if _group_alive(child.pid) and not _quiesce(child, budget):
        return 'cleanup_failed'
    if _group_alive(child.pid):
        return 'cleanup_failed'
    return outcome


def _spawn(argv: list[str], cwd: Path, streams: tuple[int, int]) -> subprocess.Popen[bytes]:
    return subprocess.Popen(  # noqa: S603 - caller is responsible for executable policy
        argv,
        cwd=cwd,
        stdin=subprocess.DEVNULL,
        stdout=streams[0],
        stderr=streams[1],
        start_new_session=True,
        env={'PATH': '/usr/bin:/bin', 'HOME': str(cwd)},
    )


def supervise(
    argv: list[str], *, options: SupervisionOptions, cancel: Event | None = None
) -> RunRecord:
    """Supervise a trusted foreground process group; never a detached Hermes gateway."""
    options.budget.validate()
    if not argv or not options.cwd.is_dir():
        msg = 'Expected a command and existing working directory'
        raise ValueError(msg)
    run_id = uuid.uuid4().hex
    directory = options.records / run_id
    directory.mkdir(parents=True, mode=0o700)
    stdout_path = directory / 'stdout.log'
    stderr_path = directory / 'stderr.log'
    started_at = datetime.now(UTC).isoformat()
    with stdout_path.open('wb') as stdout, stderr_path.open('wb') as stderr:
        child = _spawn(argv, options.cwd, (stdout.fileno(), stderr.fileno()))
        LOGGER.info('watchdog start run=%s pid=%d', run_id, child.pid)
        outcome = _monitor(child, options.budget, cancel)
        group_alive = _group_alive(child.pid)
        result = RunRecord(
            run_id=run_id,
            argv=tuple(argv),
            pid=child.pid,
            pgid=child.pid,
            started_at=started_at,
            finished_at=datetime.now(UTC).isoformat(),
            outcome=outcome,
            returncode=child.poll(),
            group_alive=group_alive,
            stdout_path=str(stdout_path),
            stderr_path=str(stderr_path),
        )
    _record(directory, result)
    LOGGER.info('watchdog stop run=%s outcome=%s alive=%s', run_id, outcome, group_alive)
    return result


# Temporary exports for legacy module import compatibility.
__all__ = [
    'LOGGER',
    'UTC',
    'Budget',
    'Event',
    'Literal',
    'Path',
    'RunRecord',
    'SupervisionOptions',
    '_group_alive',
    '_monitor',
    '_quiesce',
    '_record',
    '_signal_group',
    '_spawn',
    'asdict',
    'dataclass',
    'datetime',
    'json',
    'logging',
    'os',
    'signal',
    'subprocess',
    'supervise',
    'time',
    'uuid',
]
