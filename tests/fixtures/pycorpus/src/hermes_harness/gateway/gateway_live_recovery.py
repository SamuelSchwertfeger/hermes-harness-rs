"""Compose existing-gateway attachment with safe read-only admission recovery."""

from typing import TYPE_CHECKING

from hermes_harness.core.reconciliation import ReconciliationStatus
from hermes_harness.gateway.admission_store import LocalState
from hermes_harness.gateway.gateway_async_recovery import recover_with_async_rpc
from hermes_harness.gateway.gateway_attachment import attach_existing_gateway

if TYPE_CHECKING:
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.gateway.gateway_attachment import GatewayAttachmentConfig


async def recover_with_existing_gateway(
    store: AdmissionStore,
    submission_id: str,
    *,
    config: GatewayAttachmentConfig,
) -> ReconciliationStatus:
    """Recover through an existing authenticated gateway, without starting one.

    The durable record is checked before discovery so ineligible work never opens
    a connection. Discovery must be read-only, and the connector receives the exact
    discovered endpoint so it cannot silently start or replace a gateway.
    """
    record = store.get(submission_id)
    if record is None or record.local_state is not LocalState.OUTCOME_UNKNOWN:
        return ReconciliationStatus.NOT_ELIGIBLE

    async with attach_existing_gateway(config) as rpc:
        return await recover_with_async_rpc(store, submission_id, rpc=rpc)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'LocalState',
    'ReconciliationStatus',
    'attach_existing_gateway',
    'recover_with_async_rpc',
    'recover_with_existing_gateway',
]
