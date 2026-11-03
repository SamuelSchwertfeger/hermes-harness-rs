"""Offline-only delegated worker/reviewer workflow with durable, fail-closed stages.

This API deliberately does not start agents, execute subprocesses, merge, or grant
approval. Caller-supplied implementations must be deterministic offline doubles.
"""

import hashlib
import json
import os
import re
from dataclasses import asdict, dataclass
from enum import StrEnum
from pathlib import Path
from typing import Protocol

MAX_TEXT_CHARS = 40_000
PRIVATE_MODE = 0o600
ID_PATTERN = re.compile(r'[a-z0-9][a-z0-9-]{0,63}\Z')


class DelegationError(RuntimeError):
    """Invalid task, evidence, identity, or workflow transition."""


class Phase(StrEnum):
    CREATED = 'created'
    WORKER_DONE = 'worker_done'
    REVIEWED = 'reviewed'
    AWAITING_APPROVAL = 'awaiting_approval'


@dataclass(frozen=True, slots=True)
class Brief:
    task_id: str
    objective: str
    owned_paths: tuple[str, ...]
    worker_id: str
    reviewer_id: str


@dataclass(frozen=True, slots=True)
class Finding:
    agent_id: str
    text: str


class OfflineAgent(Protocol):
    def run(self, brief: Brief, prior: Finding | None) -> Finding:
        """Produce deterministic text evidence with no external side effects."""
        ...


def _canonical(value: object) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True).encode()


def _validate(brief: Brief) -> None:
    if not ID_PATTERN.fullmatch(brief.task_id):
        msg = 'Invalid task ID'
        raise DelegationError(msg)
    if not brief.objective.strip() or len(brief.objective) > MAX_TEXT_CHARS:
        msg = 'Invalid objective'
        raise DelegationError(msg)
    if not brief.worker_id or not brief.reviewer_id or brief.worker_id == brief.reviewer_id:
        msg = 'Worker and reviewer must have distinct identities'
        raise DelegationError(msg)
    if not brief.owned_paths:
        msg = 'At least one owned path is required'
        raise DelegationError(msg)
    for path in brief.owned_paths:
        parts = path.split('/')
        invalid_prefix = not path or path.startswith('/') or '\\' in path
        invalid_parts = any(p in ('', '.', '..') for p in parts)
        if invalid_prefix or invalid_parts:
            msg = 'Invalid owned path'
            raise DelegationError(msg)


def _write_once(path: Path, data: object) -> None:
    encoded = _canonical(data) + b'\n'
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, 'O_NOFOLLOW'):
        flags |= os.O_NOFOLLOW
    fd = os.open(path, flags, PRIVATE_MODE)
    with os.fdopen(fd, 'wb') as handle:
        handle.write(encoded)
        handle.flush()
        os.fsync(handle.fileno())


def _read(path: Path) -> dict[str, object]:
    if path.is_symlink():
        msg = 'Symlink evidence is forbidden'
        raise DelegationError(msg)
    value: object = json.loads(path.read_bytes())
    if not isinstance(value, dict):
        msg = 'Malformed evidence document'
        raise DelegationError(msg)
    return value


def _task_dir(root: Path, brief: Brief) -> Path:
    _validate(brief)
    if root.is_symlink() or not root.is_dir():
        msg = 'Root must be a real, existing directory'
        raise DelegationError(msg)
    directory = root / brief.task_id
    if directory.is_symlink():
        msg = 'Symlink task directory is forbidden'
        raise DelegationError(msg)
    return directory


def _digest(brief: Brief) -> str:
    return hashlib.sha256(_canonical(asdict(brief))).hexdigest()


def begin(root: Path, brief: Brief) -> None:
    """Persist one immutable task brief; never overwrite an earlier attempt."""
    directory = _task_dir(root, brief)
    directory.mkdir(mode=0o700, exist_ok=False)
    _write_once(directory / 'brief.json', {'digest': _digest(brief), 'brief': asdict(brief)})


def begin_reserved(root: Path, brief: Brief) -> None:
    """Persist brief in a pre-reserved task directory; refuse unreserved state."""
    directory = _task_dir(root, brief)
    if (
        not directory.is_dir()
        or (directory / 'reservation.json').is_symlink()
        or not (directory / 'reservation.json').is_file()
    ):
        msg = 'Reservation required before initializing delegated task'
        raise DelegationError(msg)
    _write_once(directory / 'brief.json', {'digest': _digest(brief), 'brief': asdict(brief)})


def phase(root: Path, brief: Brief) -> Phase:
    """Recover progress from immutable evidence, rejecting missing prerequisites."""
    directory = _task_dir(root, brief)
    saved = _read(directory / 'brief.json')
    if _canonical(saved) != _canonical({'digest': _digest(brief), 'brief': asdict(brief)}):
        msg = 'Task brief mismatch'
        raise DelegationError(msg)
    if (directory / 'approval-request.json').exists():
        if not (directory / 'review.json').exists():
            msg = 'Approval request lacks review'
            raise DelegationError(msg)
        return Phase.AWAITING_APPROVAL
    if (directory / 'review.json').exists():
        if not (directory / 'worker.json').exists():
            msg = 'Review lacks worker output'
            raise DelegationError(msg)
        return Phase.REVIEWED
    if (directory / 'worker.json').exists():
        return Phase.WORKER_DONE
    return Phase.CREATED


def _finding(agent: OfflineAgent, brief: Brief, prior: Finding | None) -> Finding:
    expected = brief.worker_id if prior is None else brief.reviewer_id
    result = agent.run(brief, prior)
    if not isinstance(result, Finding) or result.agent_id != expected:
        msg = 'Agent identity mismatch'
        raise DelegationError(msg)
    if not result.text.strip() or len(result.text) > MAX_TEXT_CHARS:
        msg = 'Empty or oversized agent output'
        raise DelegationError(msg)
    return result


def delegate(root: Path, brief: Brief, worker: OfflineAgent) -> Finding:
    """Run one injected offline worker only at CREATED stage."""
    if phase(root, brief) is not Phase.CREATED:
        msg = 'Worker already dispatched or task advanced'
        raise DelegationError(msg)
    output = _finding(worker, brief, None)
    _write_once(_task_dir(root, brief) / 'worker.json', asdict(output))
    return output


def review(root: Path, brief: Brief, reviewer: OfflineAgent) -> Finding:
    """Run a distinct injected offline reviewer against persisted worker evidence."""
    if phase(root, brief) is not Phase.WORKER_DONE:
        msg = 'Review requires completed worker evidence'
        raise DelegationError(msg)
    saved = _read(_task_dir(root, brief) / 'worker.json')
    saved_text = saved.get('text')
    if saved.get('agent_id') != brief.worker_id or not isinstance(saved_text, str):
        msg = 'Corrupt worker evidence'
        raise DelegationError(msg)
    prior = Finding(brief.worker_id, saved_text)
    output = _finding(reviewer, brief, prior)
    _write_once(_task_dir(root, brief) / 'review.json', asdict(output))
    return output


def request_approval(root: Path, brief: Brief) -> Path:
    """Record a request for human review; never grant approval or merge."""
    if phase(root, brief) is not Phase.REVIEWED:
        msg = 'Approval request requires independent review'
        raise DelegationError(msg)
    directory = _task_dir(root, brief)
    path = directory / 'approval-request.json'
    _write_once(path, {'task_digest': _digest(brief), 'approved': False})
    return path


# Temporary exports for legacy module import compatibility.
__all__ = [
    'ID_PATTERN',
    'MAX_TEXT_CHARS',
    'PRIVATE_MODE',
    'Brief',
    'DelegationError',
    'Finding',
    'OfflineAgent',
    'Path',
    'Phase',
    'Protocol',
    'StrEnum',
    '_canonical',
    '_digest',
    '_finding',
    '_read',
    '_task_dir',
    '_validate',
    '_write_once',
    'asdict',
    'begin',
    'begin_reserved',
    'dataclass',
    'delegate',
    'hashlib',
    'json',
    'os',
    'phase',
    're',
    'request_approval',
    'review',
]
