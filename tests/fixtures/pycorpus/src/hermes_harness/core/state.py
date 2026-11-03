"""Persistent ticket state with guarded transitions and explicit human approvals."""

import json
import os
import re
import tempfile
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime
from enum import StrEnum
from pathlib import Path
from typing import Any


class Stage(StrEnum):
    INVESTIGATE = 'investigate'
    FACTS = 'consolidate-facts'
    TICKET = 'open-ticket'
    PLAN = 'plan'
    IMPLEMENT = 'approved-implementation'
    VERIFY = 'deterministic-verify'
    REVIEW = 'independent-review'
    PR = 'ci-and-pr'
    HUMAN_REVIEW = 'human-review'
    ARCHIVE = 'archive-and-baton-pass'


NEXT: dict[Stage, set[Stage]] = {
    Stage.INVESTIGATE: {Stage.FACTS},
    Stage.FACTS: {Stage.TICKET},
    Stage.TICKET: {Stage.PLAN},
    Stage.PLAN: {Stage.IMPLEMENT},
    Stage.IMPLEMENT: {Stage.VERIFY},
    Stage.VERIFY: {Stage.REVIEW, Stage.IMPLEMENT},
    Stage.REVIEW: {Stage.PR, Stage.PLAN},
    Stage.PR: {Stage.HUMAN_REVIEW, Stage.IMPLEMENT},
    Stage.HUMAN_REVIEW: {Stage.ARCHIVE, Stage.PLAN},
    Stage.ARCHIVE: set(),
}

APPROVAL_GATES = {Stage.IMPLEMENT: 'plan', Stage.ARCHIVE: 'merge'}


@dataclass
class Ticket:
    id: str
    summary: str
    stage: Stage = Stage.INVESTIGATE
    allowed_paths: list[str] = field(default_factory=list)
    approvals: dict[str, bool] = field(default_factory=dict)
    evidence: dict[str, str] = field(default_factory=dict)
    history: list[dict[str, str]] = field(default_factory=list)

    def advance(self, target: Stage) -> None:
        if target not in NEXT[self.stage]:
            msg = f'Transition forbidden: {self.stage} -> {target}'
            raise ValueError(msg)
        gate = APPROVAL_GATES.get(target)
        if gate and not self.approvals.get(gate, False):
            msg = f'Human approval required: {gate}'
            raise PermissionError(msg)
        required = {
            Stage.FACTS: 'facts',
            Stage.PLAN: 'plan',
            Stage.VERIFY: 'commit',
            Stage.REVIEW: 'verification',
            Stage.PR: 'review',
            Stage.HUMAN_REVIEW: 'ci',
            Stage.ARCHIVE: 'merged_pr',
        }.get(target)
        if required and not self.evidence.get(required):
            msg = f'Missing evidence: {required}'
            raise ValueError(msg)
        old = self.stage
        self.stage = target
        self.history.append({
            'at': datetime.now(UTC).isoformat(),
            'from': old.value,
            'to': target.value,
        })


def ticket_path(root: Path, ticket_id: str) -> Path:
    if not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', ticket_id):
        msg = 'Ticket ID must be a lower-case slug'
        raise ValueError(msg)
    return root / 'tickets' / ticket_id / 'state.json'


def save(root: Path, ticket: Ticket) -> None:
    path = ticket_path(root, ticket.id)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    payload = json.dumps(asdict(ticket), indent=2) + '\n'
    fd, name = tempfile.mkstemp(prefix='.state-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        Path(name).chmod(0o600)
        Path(name).replace(path)
    finally:
        if Path(name).exists():
            Path(name).unlink()


def load(root: Path, ticket_id: str) -> Ticket:
    data: dict[str, Any] = json.loads(ticket_path(root, ticket_id).read_text())
    data['stage'] = Stage(data['stage'])
    if data['id'] != ticket_id:
        msg = 'Ticket ID mismatch'
        raise ValueError(msg)
    return Ticket(**data)


def create(root: Path, ticket_id: str, summary: str) -> Ticket:
    path = ticket_path(root, ticket_id)
    if path.exists():
        raise FileExistsError(path)
    ticket = Ticket(id=ticket_id, summary=summary)
    save(root, ticket)
    base = path.parent
    for dirname in ('briefs', 'review', 'handoffs', 'runs'):
        (base / dirname).mkdir(exist_ok=True)
    for name, heading in (
        ('facts.md', 'Facts and unresolved questions'),
        ('plan.md', 'Implementation plan and verification contract'),
        ('REPORT.md', 'Outcome report'),
    ):
        (base / name).write_text(f'# {heading}\n', encoding='utf-8')
    return ticket


# Temporary exports for legacy module import compatibility.
__all__ = [
    'APPROVAL_GATES',
    'NEXT',
    'UTC',
    'Any',
    'Path',
    'Stage',
    'StrEnum',
    'Ticket',
    'asdict',
    'create',
    'dataclass',
    'datetime',
    'field',
    'json',
    'load',
    'os',
    're',
    'save',
    'tempfile',
    'ticket_path',
]
