"""Fail-closed Hermes invocation planning; no process or gateway is launched.

Hermes CLI lifetime cannot be used as proof of completion of gateway-owned work.
Keep execution disabled until the installed runtime exposes verifiable cancellation.
"""

import json
from dataclasses import asdict, dataclass
from enum import StrEnum
from pathlib import Path

MAX_PROMPT_CHARS = 8_000


class HermesSafetyError(RuntimeError):
    """An unsafe or unverified Hermes action was requested."""


class TaskKind(StrEnum):
    SCOUT = 'scout'
    PLAN = 'plan'
    REVIEW = 'review'
    CYBER = 'cyber'


@dataclass(frozen=True, slots=True, kw_only=True)
class TaskRequest:
    ticket_id: str
    kind: TaskKind
    prompt: str
    workspace: Path
    deadline_seconds: int = 60
    max_turns: int = 2
    authorized_scope: str | None = None

    def validate(self) -> None:
        if not self.ticket_id or '/' in self.ticket_id or '..' in self.ticket_id:
            msg = 'Invalid ticket ID'
            raise ValueError(msg)
        if not self.prompt.strip() or len(self.prompt) > MAX_PROMPT_CHARS:
            msg = 'Prompt must contain 1 to 8000 characters'
            raise ValueError(msg)
        if self.deadline_seconds <= 0 or self.max_turns <= 0:
            msg = 'Time and turn budgets must be positive'
            raise ValueError(msg)
        if not self.workspace.is_absolute() or not self.workspace.is_dir():
            msg = 'Workspace must be an existing absolute directory'
            raise ValueError(msg)
        if self.kind is TaskKind.CYBER and not self.authorized_scope:
            msg = 'Cyber tasks require an explicit authorized scope'
            raise PermissionError(msg)


@dataclass(frozen=True, slots=True)
class PreparedInvocation:
    ticket_id: str
    kind: str
    provider: str
    model: str
    max_turns: int
    deadline_seconds: int
    workspace: str
    prompt: str
    execution_enabled: bool = False
    disabled_reason: str = 'Gateway job identity and cancellation not verified'

    def save(self, destination: Path) -> None:
        """Persist a plan, never any Hermes credentials or full configuration."""
        if destination.exists():
            msg = 'Refusing to overwrite existing invocation manifest'
            raise FileExistsError(msg)
        destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        # Prompts may contain private task material; manifests are private by default.
        with destination.open('x', encoding='utf-8') as stream:
            destination.chmod(0o600)
            stream.write(json.dumps(asdict(self), indent=2) + '\n')


ROUTES: dict[TaskKind, tuple[str, str]] = {
    TaskKind.SCOUT: ('ollama', 'granite4:3b'),
    TaskKind.PLAN: ('openai-codex', 'gpt-6.1-sol'),
    TaskKind.REVIEW: ('openai-codex', 'gpt-6.1-sol'),
}


def prepare(request: TaskRequest) -> PreparedInvocation:
    """Prepare an audited task request; no model calls or shell execution."""
    request.validate()
    if request.kind is TaskKind.CYBER:
        msg = 'Cyber adapter disabled pending authorization and model benchmark'
        raise HermesSafetyError(msg)
    provider, model = ROUTES[request.kind]
    return PreparedInvocation(
        ticket_id=request.ticket_id,
        kind=request.kind.value,
        provider=provider,
        model=model,
        max_turns=request.max_turns,
        deadline_seconds=request.deadline_seconds,
        workspace=str(request.workspace.resolve()),
        prompt=request.prompt,
    )


def execute(invocation: PreparedInvocation) -> None:
    """Hard-stop until tested gateway-aware lifecycle controls are installed."""
    msg = f'Hermes execution disabled: {invocation.disabled_reason}'
    raise HermesSafetyError(msg)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_PROMPT_CHARS',
    'ROUTES',
    'HermesSafetyError',
    'Path',
    'PreparedInvocation',
    'StrEnum',
    'TaskKind',
    'TaskRequest',
    'asdict',
    'dataclass',
    'execute',
    'json',
    'prepare',
]
