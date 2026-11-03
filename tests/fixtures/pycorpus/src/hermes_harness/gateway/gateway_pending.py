"""Read-only pending-admission identity recovery from a replay-derived snapshot.

A snapshot is never evidence of absence or completed execution. No RPC is sent
by this module; a caller supplies a previously authenticated SessionSnapshot.
"""

from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from hermes_harness.gateway.gateway_readonly import SessionSnapshot


class PendingDisposition(StrEnum):
    """Conservative classification of a snapshot's matching pending entries."""

    FOUND = 'found'
    INDETERMINATE = 'indeterminate'


@dataclass(frozen=True, slots=True)
class PendingLookup:
    """A candidate admission identity, not a completion or cancellation receipt."""

    disposition: PendingDisposition
    admission_id: str | None = None


def _matches(pending: object, submission_id: str) -> list[dict[str, object]] | None:
    if not isinstance(pending, list):
        return None
    matches: list[dict[str, object]] = []
    for item in pending:
        if not isinstance(item, dict):
            return None
        if item.get('input_id') != submission_id:
            continue
        if any(not isinstance(key, str) for key in item):
            return None
        matches.append(item)
    return matches


def inspect_pending(
    snapshot: SessionSnapshot, session_id: str, submission_id: str
) -> PendingLookup:
    """Extract exactly one matching pending admission from an existing snapshot.

    Fail closed on malformed snapshots and multiple matches. An empty pending
    list provides no proof of non-admission: terminal work may be absent.
    """
    unknown = PendingLookup(PendingDisposition.INDETERMINATE)
    if not session_id.strip() or not submission_id.strip() or snapshot.session_id != session_id:
        return unknown
    matches = _matches(snapshot.data.get('pending'), submission_id)
    if matches is None or len(matches) != 1:
        return unknown
    admission_id = matches[0].get('admission_id')
    if not isinstance(admission_id, str) or not admission_id.strip():
        return unknown
    return PendingLookup(PendingDisposition.FOUND, admission_id)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'PendingDisposition',
    'PendingLookup',
    'StrEnum',
    '_matches',
    'dataclass',
    'inspect_pending',
]
