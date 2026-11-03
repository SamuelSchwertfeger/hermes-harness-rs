"""Explicit model registry: selection never grants tool or file privileges."""

from dataclasses import dataclass
from enum import StrEnum


class Role(StrEnum):
    PLANNER = 'planner'
    ENGINEER = 'engineer'
    SCOUT = 'scout'
    REVIEWER = 'reviewer'
    CYBER = 'cyber'


@dataclass(frozen=True, kw_only=True)
class Route:
    role: Role
    backend: str
    model: str
    tool_access: bool
    read_only: bool
    max_seconds: int
    max_output_tokens: int
    gpu_slots: int
    enabled: bool = True


ROUTES: dict[Role, Route] = {
    Role.PLANNER: Route(
        role=Role.PLANNER,
        backend='codex',
        model='gpt-6.1-sol',
        tool_access=False,
        read_only=True,
        max_seconds=120,
        max_output_tokens=2048,
        gpu_slots=0,
        enabled=True,
    ),
    Role.ENGINEER: Route(
        role=Role.ENGINEER,
        backend='codex',
        model='gpt-6.1-sol',
        tool_access=False,
        read_only=False,
        max_seconds=180,
        max_output_tokens=3072,
        gpu_slots=0,
        enabled=True,
    ),
    Role.SCOUT: Route(
        role=Role.SCOUT,
        backend='ollama',
        model='granite4:3b',
        tool_access=False,
        read_only=True,
        max_seconds=60,
        max_output_tokens=512,
        gpu_slots=1,
        enabled=True,
    ),
    Role.REVIEWER: Route(
        role=Role.REVIEWER,
        backend='ollama',
        model='granite4:3b',
        tool_access=False,
        read_only=True,
        max_seconds=90,
        max_output_tokens=1024,
        gpu_slots=1,
        enabled=True,
    ),
    Role.CYBER: Route(
        role=Role.CYBER,
        backend='unconfigured',
        model='',
        tool_access=False,
        read_only=True,
        max_seconds=60,
        max_output_tokens=1024,
        gpu_slots=1,
        enabled=False,
    ),
}


def select(role: Role, *, authorized_target: bool = False) -> Route:
    route = ROUTES[role]
    if not route.enabled:
        msg = f'Route {role} is disabled pending evaluation'
        raise PermissionError(msg)
    if role is Role.CYBER and not authorized_target:
        msg = 'Cyber operations require an authorized target'
        raise PermissionError(msg)
    return route


# Temporary exports for legacy module import compatibility.
__all__ = [
    'ROUTES',
    'Role',
    'Route',
    'StrEnum',
    'dataclass',
    'select',
]
