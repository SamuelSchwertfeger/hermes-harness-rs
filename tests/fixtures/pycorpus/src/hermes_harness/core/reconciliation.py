"""Fail-closed reconciliation for submissions with uncertain gateway outcomes.

This module does not implement a Hermes RPC method. A caller must inject a resolver
whose lookup-by-submission-ID semantics have been verified against the installed
Hermes gateway. Missing or ambiguous results never authorize resubmission.
"""

from dataclasses import dataclass
from enum import StrEnum
from typing import TYPE_CHECKING, Protocol

from hermes_harness.gateway.admission_store import LocalState

if TYPE_CHECKING:
    from hermes_harness.gateway.admission_store import AdmissionRecord, AdmissionStore
    from hermes_harness.gateway.gateway_identity import Receipt


class ReconciliationError(RuntimeError):
    """A resolver returned a result that violates the reconciliation contract."""


class LookupDisposition(StrEnum):
    """Meaning of a resolver's authoritative lookup result."""

    FOUND = 'found'
    NOT_FOUND = 'not_found'
    INDETERMINATE = 'indeterminate'


class ReconciliationStatus(StrEnum):
    """Harness result; deliberately not a Hermes execution status."""

    RECEIPT_RECORDED = 'receipt_recorded'
    STILL_UNKNOWN = 'still_unknown'
    NOT_ELIGIBLE = 'not_eligible'


@dataclass(frozen=True, slots=True)
class LookupResult:
    """Result of a verified lookup by stable submission identity."""

    disposition: LookupDisposition
    receipt: Receipt | None = None

    def __post_init__(self) -> None:
        if (self.disposition is LookupDisposition.FOUND) != (self.receipt is not None):
            msg = 'A receipt is required only when disposition is FOUND'
            raise ValueError(msg)


class SubmissionResolver(Protocol):
    """Injected lookup adapter; it must not submit, retry, or start Hermes."""

    def resolve(self, record: AdmissionRecord) -> LookupResult:
        """Look up the existing request by session and stable submission ID."""
        ...


def reconcile_unknown(
    store: AdmissionStore,
    submission_id: str,
    *,
    resolver: SubmissionResolver,
) -> ReconciliationStatus:
    """Reconcile an uncertain request without retrying or guessing.

    Only records in ``OUTCOME_UNKNOWN`` are eligible. A verified receipt is persisted
    with ``reconciled=True``. NOT_FOUND and INDETERMINATE leave the record unknown:
    absence from a lookup is not proof that the original request was never admitted.
    Resolver exceptions also propagate while the durable record remains unchanged.
    """
    record = store.get(submission_id)
    if record is None or record.local_state is not LocalState.OUTCOME_UNKNOWN:
        return ReconciliationStatus.NOT_ELIGIBLE

    result = resolver.resolve(record)
    if not isinstance(result, LookupResult):
        msg = 'Resolver must return a LookupResult'
        raise ReconciliationError(msg)

    if result.disposition is not LookupDisposition.FOUND:
        return ReconciliationStatus.STILL_UNKNOWN

    receipt = result.receipt
    if receipt is None:
        msg = 'FOUND lookup result is missing its receipt'
        raise ReconciliationError(msg)
    if record.admission_id is not None and record.admission_id != receipt.admission_id:
        msg = 'Resolved receipt conflicts with the durable admission ID'
        raise ReconciliationError(msg)

    store.record_receipt(submission_id, receipt, reconciled=True)
    return ReconciliationStatus.RECEIPT_RECORDED


# Temporary exports for legacy module import compatibility.
__all__ = [
    'LocalState',
    'LookupDisposition',
    'LookupResult',
    'Protocol',
    'ReconciliationError',
    'ReconciliationStatus',
    'StrEnum',
    'SubmissionResolver',
    'dataclass',
    'reconcile_unknown',
]
