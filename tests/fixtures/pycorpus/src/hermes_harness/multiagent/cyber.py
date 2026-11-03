"""Explicit authorization boundary for optional Cyber Warrior worker roles.

This gate does not grant process isolation or enable any live model.
Security-role execution requires an operator-bound repository, task and path grant.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from pathlib import Path


class CyberAuthorizationError(RuntimeError):
    """Missing or excessively broad operator authorization."""


@dataclass(frozen=True, slots=True)
class CyberGrant:
    """Operator-authorized scope for a local security specialist."""

    repository: Path
    task_ids: tuple[str, ...]
    owned_paths: tuple[str, ...]
    approved_by: str


def validate_cyber_node(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    node_id: str, owned_paths: tuple[str, ...], repository: Path, grant: CyberGrant | None
) -> None:
    """Reject unapproved Cyber Warrior dispatch before creating task state."""
    if grant is None:
        msg = 'Cyber Warrior requires an explicit operator grant'
        raise CyberAuthorizationError(msg)
    if not grant.approved_by.strip() or grant.repository.is_symlink():
        msg = 'Invalid Cyber Warrior grant identity or repository'
        raise CyberAuthorizationError(msg)
    if repository.resolve() != grant.repository.resolve():
        msg = 'Cyber Warrior grant repository mismatch'
        raise CyberAuthorizationError(msg)
    if node_id not in grant.task_ids:
        msg = 'Cyber Warrior task not authorized'
        raise CyberAuthorizationError(msg)
    if not set(owned_paths).issubset(set(grant.owned_paths)):
        msg = 'Cyber Warrior ownership exceeds grant'
        raise CyberAuthorizationError(msg)
