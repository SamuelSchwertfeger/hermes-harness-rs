"""Fail-closed, auditable orchestration of *injected* offline worker results.

No Hermes or Ollama network adapter is authorized by this module. A caller must
supply an explicitly trusted Worker implementation. The default has no dispatch.
"""

import json
import os
import re
from dataclasses import asdict, dataclass
from datetime import UTC, datetime
from enum import StrEnum
from pathlib import Path
from typing import TYPE_CHECKING, Protocol
from uuid import uuid4

from hermes_harness.core.state import Stage, load
from hermes_harness.gateway.hermes_adapter import (
    TaskKind,
    TaskRequest,
    prepare,
)

if TYPE_CHECKING:
    from hermes_harness.gateway.hermes_adapter import (
        PreparedInvocation,
    )

MAX_RESPONSE_CHARS = 40_000


class TaskStatus(StrEnum):
    SUCCEEDED = 'succeeded'
    FAILED = 'failed'


class DispatchError(RuntimeError):
    """Worker dispatch forbidden or evidence validation failed."""


class Worker(Protocol):
    """Trusted offline worker seam; never automatically constructs a live backend."""

    def run(self, invocation: PreparedInvocation) -> str:
        """Return bounded, tool-free text for a single approved invocation."""
        ...


@dataclass(frozen=True, slots=True, kw_only=True)
class TaskSpec:
    ticket_id: str
    kind: TaskKind
    prompt: str
    workspace: Path
    deadline_seconds: int = 30
    max_turns: int = 2


@dataclass(frozen=True, slots=True, kw_only=True)
class TaskResult:
    run_id: str
    ticket_id: str
    kind: str
    provider: str
    model: str
    status: TaskStatus
    output_path: str
    timestamp: str


_ALLOWED_STAGES: dict[TaskKind, frozenset[Stage]] = {
    TaskKind.SCOUT: frozenset({Stage.INVESTIGATE, Stage.FACTS}),
    TaskKind.PLAN: frozenset({Stage.TICKET, Stage.PLAN}),
    TaskKind.REVIEW: frozenset({Stage.REVIEW}),
}


def _save_private(path: Path, content: str) -> None:
    """Create without overwrite and avoid exposing private task data to other users."""
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def _validate_output(output: object) -> str:
    if not isinstance(output, str):
        msg = 'Worker output must be a string'
        raise DispatchError(msg)
    if not output.strip() or len(output) > MAX_RESPONSE_CHARS:
        msg = 'Worker output missing or over byte-independent character budget'
        raise DispatchError(msg)
    return output


def _preflight(root: Path, spec: TaskSpec) -> PreparedInvocation:
    ticket = load(root, spec.ticket_id)
    if spec.kind is TaskKind.CYBER or spec.kind not in _ALLOWED_STAGES:
        msg = 'Cyber and unknown workers are not authorized'
        raise DispatchError(msg)
    if ticket.stage not in _ALLOWED_STAGES[spec.kind]:
        msg = f'Worker {spec.kind} not allowed at ticket stage {ticket.stage}'
        raise DispatchError(msg)
    if not spec.workspace.is_dir() or not spec.workspace.is_absolute():
        msg = 'Workspace must be an existing absolute directory'
        raise DispatchError(msg)
    if not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', spec.ticket_id):
        msg = 'Invalid ticket ID'
        raise DispatchError(msg)
    return prepare(
        TaskRequest(
            ticket_id=spec.ticket_id,
            kind=spec.kind,
            prompt=spec.prompt,
            workspace=spec.workspace,
            deadline_seconds=spec.deadline_seconds,
            max_turns=spec.max_turns,
        )
    )


def dispatch_offline(root: Path, spec: TaskSpec, worker: Worker) -> TaskResult:
    """Run a trusted mock/offline worker and retain a private auditable result.

    This is NOT a timeout or sandbox boundary. Live adapters must not use this API
    until gateway cancellation and enforced deadline controls are integrated.
    """
    invocation = _preflight(root, spec)
    run_id = uuid4().hex
    directory = root / 'tickets' / spec.ticket_id / 'runs' / run_id
    directory.mkdir(parents=True, mode=0o700, exist_ok=False)
    invocation.save(directory / 'request.json')
    try:
        output = _validate_output(worker.run(invocation))
    except Exception as exc:
        failure = {'status': TaskStatus.FAILED, 'error_type': type(exc).__name__}
        _save_private(directory / 'failure.json', json.dumps(failure, indent=2) + '\n')
        raise
    output_path = directory / 'output.txt'
    _save_private(output_path, output)
    result = TaskResult(
        run_id=run_id,
        ticket_id=spec.ticket_id,
        kind=spec.kind.value,
        provider=invocation.provider,
        model=invocation.model,
        status=TaskStatus.SUCCEEDED,
        output_path=str(output_path),
        timestamp=datetime.now(UTC).isoformat(),
    )
    _save_private(directory / 'result.json', json.dumps(asdict(result), indent=2) + '\n')
    return result


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_RESPONSE_CHARS',
    'UTC',
    '_ALLOWED_STAGES',
    'DispatchError',
    'Path',
    'Protocol',
    'Stage',
    'StrEnum',
    'TaskKind',
    'TaskRequest',
    'TaskResult',
    'TaskSpec',
    'TaskStatus',
    'Worker',
    '_preflight',
    '_save_private',
    '_validate_output',
    'asdict',
    'dataclass',
    'datetime',
    'dispatch_offline',
    'json',
    'load',
    'os',
    'prepare',
    're',
    'uuid4',
]
