"""Receipt reconciliation using only Hermes's known-admission receipt RPC.

This adapter cannot recover a lost admission ID. When that ID is unknown, it
returns INDETERMINATE without making an RPC call. It never submits or retries work.
"""

from typing import Protocol

from hermes_harness.core.reconciliation import LookupDisposition, LookupResult
from hermes_harness.gateway.gateway_identity import parse_receipt


class AdmissionLike(Protocol):
    """Minimum durable identity needed for a receipt lookup."""

    @property
    def session_id(self) -> str:
        """Session owning this admission."""
        ...

    @property
    def admission_id(self) -> str | None:
        """Existing admission identity, when known."""
        ...


class GatewayRPC(Protocol):
    """Injected RPC transport for an already authenticated, existing gateway."""

    def call(self, method: str, params: dict[str, str]) -> object:
        """Make one RPC call and return its untrusted decoded response."""
        ...


class KnownAdmissionReceiptResolver:
    """Resolve receipt state only when the durable ledger already knows admission_id."""

    def __init__(self, rpc: GatewayRPC) -> None:
        self._rpc = rpc

    def resolve(self, record: AdmissionLike) -> LookupResult:
        """Fetch the existing receipt or return indeterminate without guessing."""
        admission_id = record.admission_id
        if admission_id is None:
            return LookupResult(LookupDisposition.INDETERMINATE)

        response = self._rpc.call(
            'prompt.receipt',
            {'session_id': record.session_id, 'admission_id': admission_id},
        )
        receipt = parse_receipt(response, expected_admission_id=admission_id)
        return LookupResult(LookupDisposition.FOUND, receipt)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'AdmissionLike',
    'GatewayRPC',
    'KnownAdmissionReceiptResolver',
    'LookupDisposition',
    'LookupResult',
    'Protocol',
    'parse_receipt',
]
