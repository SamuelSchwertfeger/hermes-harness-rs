"""Supervised foreground fixture transport; live model executables remain disabled."""

import json
import os
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING
from uuid import uuid4

from hermes_harness.core.watchdog import Budget, SupervisionOptions, supervise

if TYPE_CHECKING:
    from threading import Event

    from hermes_harness.gateway.hermes_adapter import PreparedInvocation

MAX_OUTPUT_BYTES = 40_000
FORBIDDEN_EXECUTABLES = frozenset({'hermes', 'codex', 'ollama'})


class TransportError(RuntimeError):
    """Foreground fixture failed or violated its execution contract."""


@dataclass(frozen=True, slots=True, kw_only=True)
class ForegroundOptions:
    """Runner selected by trusted harness code, never by a model response."""

    argv: tuple[str, ...]
    records: Path
    budget: Budget = field(default_factory=Budget)

    def validate(self) -> None:
        if not self.argv or not Path(self.argv[0]).is_absolute():
            msg = 'Fixture executable must use an absolute path'
            raise ValueError(msg)
        if Path(self.argv[0]).name.lower() in FORBIDDEN_EXECUTABLES:
            msg = 'Live model executables are not supported by fixture transport'
            raise PermissionError(msg)
        if not Path(self.argv[0]).is_file():
            msg = 'Fixture executable does not exist'
            raise ValueError(msg)
        self.budget.validate()


class ForegroundFixtureWorker:
    """Protocol-compatible worker for trusted fixtures, not live inference."""

    def __init__(self, options: ForegroundOptions, *, cancel: Event | None = None) -> None:
        options.validate()
        self._options = options
        self._cancel = cancel

    def _write_request(self, invocation: PreparedInvocation) -> tuple[Path, Path]:
        if invocation.execution_enabled:
            msg = 'Live-enabled invocations are prohibited'
            raise TransportError(msg)
        workspace = Path(invocation.workspace).resolve(strict=True)
        if not workspace.is_dir():
            msg = 'Workspace is not a directory'
            raise TransportError(msg)
        directory = self._options.records / f'fixture-{uuid4().hex}'
        directory.mkdir(parents=True, mode=0o700, exist_ok=False)
        request = directory / 'request.json'
        fd = os.open(request, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, 'w', encoding='utf-8') as stream:
            json.dump(asdict(invocation), stream)
        return workspace, request

    def _execute(self, workspace: Path, request: Path, deadline: int) -> Path:
        budget = self._options.budget
        effective_budget = Budget(
            deadline_seconds=min(budget.deadline_seconds, deadline),
            grace_seconds=budget.grace_seconds,
            poll_seconds=budget.poll_seconds,
        )
        record = supervise(
            [*self._options.argv, str(request)],
            options=SupervisionOptions(
                cwd=workspace, records=request.parent, budget=effective_budget
            ),
            cancel=self._cancel,
        )
        if record.outcome != 'completed' or record.returncode != 0 or record.group_alive:
            msg = f'Fixture terminated without success: {record.outcome}'
            raise TransportError(msg)
        return Path(record.stdout_path)

    @staticmethod
    def _read_output(output: Path) -> str:
        with output.open('rb') as stream:
            payload = stream.read(MAX_OUTPUT_BYTES + 1)
        if len(payload) > MAX_OUTPUT_BYTES:
            msg = 'Fixture output exceeds byte budget'
            raise TransportError(msg)
        try:
            result = payload.decode('utf-8')
        except UnicodeDecodeError as exc:
            msg = 'Fixture emitted invalid UTF-8'
            raise TransportError(msg) from exc
        if not result.strip():
            msg = 'Fixture output is empty'
            raise TransportError(msg)
        return result

    def run(self, invocation: PreparedInvocation) -> str:
        """Run a trusted fixture using the bounded watchdog and return its text."""
        workspace, request = self._write_request(invocation)
        output = self._execute(workspace, request, invocation.deadline_seconds)
        return self._read_output(output)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'FORBIDDEN_EXECUTABLES',
    'MAX_OUTPUT_BYTES',
    'Budget',
    'ForegroundFixtureWorker',
    'ForegroundOptions',
    'Path',
    'SupervisionOptions',
    'TransportError',
    'asdict',
    'dataclass',
    'field',
    'json',
    'os',
    'supervise',
    'uuid4',
]
