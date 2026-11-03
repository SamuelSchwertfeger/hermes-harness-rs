"""Fail-closed recovery of uncertain admissions using existing read-only RPCs.

The injected transports must already be authenticated. This module never submits,
starts a gateway, retries a request, or treats pending-list absence as rejection.
"""

from dataclasses import dataclass, replace
from typing import TYPE_CHECKING

from hermes_harness.core.reconciliation import ReconciliationStatus, reconcile_unknown
from hermes_harness.gateway.admission_store import LocalState
from hermes_harness.gateway.gateway_event_replay import read_pending_snapshot
from hermes_harness.gateway.gateway_pending import PendingDisposition, inspect_pending
from hermes_harness.gateway.gateway_resolver import KnownAdmissionReceiptResolver

if TYPE_CHECKING:
    from hermes_harness.core.reconciliation import LookupResult
    from hermes_harness.gateway.admission_store import AdmissionRecord, AdmissionStore
    from hermes_harness.gateway.gateway_readonly import GatewayRPC as ReadRPC
    from hermes_harness.gateway.gateway_resolver import GatewayRPC as ReceiptRPC


@dataclass(frozen=True, slots=True)
class RecoveryClients:
    """Already-authenticated, injected read-only gateway clients."""

    read_rpc: ReadRPC
    receipt_rpc: ReceiptRPC


class _CandidateReceiptResolver:
    """Bridge a snapshot candidate to the existing validated receipt resolver."""

    def __init__(self, rpc: ReceiptRPC, candidate_id: str) -> None:
        self._resolver = KnownAdmissionReceiptResolver(rpc)
        self._candidate_id = candidate_id

    def resolve(self, record: AdmissionRecord) -> LookupResult:
        """Do exactly one receipt lookup with the candidate, never a submit."""
        return self._resolver.resolve(replace(record, admission_id=self._candidate_id))


async def recover_unknown_admission(
    store: AdmissionStore,
    submission_id: str,
    *,
    clients: RecoveryClients,
) -> ReconciliationStatus:
    """Reconcile an unknown record without ever redispatching its submission.

    Known admission IDs need only a receipt lookup. Otherwise inspect a complete,
    epoch-bound ``session.events.since`` replay for a *candidate* admission ID,
    then validate the candidate with ``prompt.receipt`` before mutating the ledger.
    Any uncertainty leaves the durable record unchanged.
    """
    record = store.get(submission_id)
    if record is None or record.local_state is not LocalState.OUTCOME_UNKNOWN:
        return ReconciliationStatus.NOT_ELIGIBLE

    if record.admission_id is not None:
        return reconcile_unknown(
            store, submission_id, resolver=KnownAdmissionReceiptResolver(clients.receipt_rpc)
        )

    snapshot = await read_pending_snapshot(clients.read_rpc, record.session_id)
    if snapshot is None:
        return ReconciliationStatus.STILL_UNKNOWN
    candidate = inspect_pending(snapshot, record.session_id, record.submission_id)
    if candidate.disposition is not PendingDisposition.FOUND or candidate.admission_id is None:
        return ReconciliationStatus.STILL_UNKNOWN

    return reconcile_unknown(
        store,
        submission_id,
        resolver=_CandidateReceiptResolver(clients.receipt_rpc, candidate.admission_id),
    )


# Temporary exports for legacy module import compatibility.
__all__ = [
    'KnownAdmissionReceiptResolver',
    'LocalState',
    'PendingDisposition',
    'ReconciliationStatus',
    'RecoveryClients',
    '_CandidateReceiptResolver',
    'dataclass',
    'inspect_pending',
    'read_pending_snapshot',
    'reconcile_unknown',
    'recover_unknown_admission',
    'replace',
]
