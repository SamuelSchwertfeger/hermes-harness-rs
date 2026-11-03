"""Read-only recovery through one already authenticated async Hermes RPC connection.

The injected RPC must attach to an existing gateway. This module does not import
Hermes, discover endpoints, start services, submit prompts, or retry failed RPCs.
"""

from typing import TYPE_CHECKING, Protocol

from hermes_harness.core.reconciliation import (
    LookupDisposition,
    LookupResult,
    ReconciliationStatus,
    reconcile_unknown,
)
from hermes_harness.gateway.admission_store import LocalState
from hermes_harness.gateway.gateway_event_replay import read_pending_snapshot
from hermes_harness.gateway.gateway_identity import parse_receipt
from hermes_harness.gateway.gateway_pending import PendingDisposition, inspect_pending

if TYPE_CHECKING:
    from hermes_harness.gateway.admission_store import AdmissionStore


class AsyncRecoveryRPC(Protocol):
    """An already-authenticated gateway connection, injected by the caller."""

    async def rpc(self, method: str, **params: object) -> object:
        """Execute exactly one RPC, without reconnecting or retrying."""
        ...


class _VerifiedReceipt:
    """One-shot adapter for the existing durable reconciliation transaction."""

    def __init__(self, result: LookupResult) -> None:
        self._result = result

    def resolve(self, _record: object) -> LookupResult:
        """Return only the already validated server receipt; perform no I/O."""
        return self._result


async def recover_with_async_rpc(
    store: AdmissionStore,
    submission_id: str,
    *,
    rpc: AsyncRecoveryRPC,
) -> ReconciliationStatus:
    """Read a pending candidate, verify its receipt, and persist it if authoritative.

    Existing admission IDs skip event replay. Unknown IDs require exactly one
    matching pending entry from a complete ``session.events.since`` replay;
    absence is never proof of non-admission. Failures leave the ledger unchanged.
    """
    record = store.get(submission_id)
    if record is None or record.local_state is not LocalState.OUTCOME_UNKNOWN:
        return ReconciliationStatus.NOT_ELIGIBLE

    admission_id = record.admission_id
    if admission_id is None:
        snapshot = await read_pending_snapshot(rpc, record.session_id)
        if snapshot is None:
            return ReconciliationStatus.STILL_UNKNOWN
        candidate = inspect_pending(snapshot, record.session_id, record.submission_id)
        if candidate.disposition is not PendingDisposition.FOUND or candidate.admission_id is None:
            return ReconciliationStatus.STILL_UNKNOWN
        admission_id = candidate.admission_id

    raw = await rpc.rpc('prompt.receipt', session_id=record.session_id, admission_id=admission_id)
    receipt = parse_receipt(raw, expected_admission_id=admission_id)
    result = LookupResult(LookupDisposition.FOUND, receipt)
    return reconcile_unknown(store, submission_id, resolver=_VerifiedReceipt(result))


# Temporary exports for legacy module import compatibility.
__all__ = [
    'AsyncRecoveryRPC',
    'LocalState',
    'LookupDisposition',
    'LookupResult',
    'PendingDisposition',
    'Protocol',
    'ReconciliationStatus',
    '_VerifiedReceipt',
    'inspect_pending',
    'parse_receipt',
    'read_pending_snapshot',
    'reconcile_unknown',
    'recover_with_async_rpc',
]
