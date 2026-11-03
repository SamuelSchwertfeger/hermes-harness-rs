"""Compose one disposable-session creation with one admission on the same RPC.

This module never discovers/starts Hermes. Its caller must explicitly supply an
existing authenticated connection; it does not retry either operation.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.gateway.gateway_identity import Submission
from hermes_harness.gateway.gateway_live_admission import (
    LiveAdmissionRequest,
    submit_once,
)
from hermes_harness.gateway.gateway_session_creation import (
    prepare_disposable_session,
)

if TYPE_CHECKING:
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.gateway.gateway_live_admission import LiveAdmissionResult
    from hermes_harness.gateway.gateway_session_creation import (
        PreparedSession,
        SessionCreationRequest,
        SessionRPC,
    )


@dataclass(frozen=True, slots=True)
class DisposableAdmissionRequest:
    """Operator-chosen identities plus an immutable harmless prompt."""

    creation: SessionCreationRequest
    submission_id: str
    text: str


@dataclass(frozen=True, slots=True)
class DisposableAdmissionResult:
    """Identities verified on the same gateway connection."""

    prepared: PreparedSession
    admission: LiveAdmissionResult


def _validate(request: DisposableAdmissionRequest, store: AdmissionStore) -> None:
    """Reject invalid or already-used identities before any creation RPC."""
    if not isinstance(request.submission_id, str) or not request.submission_id.strip():
        msg = 'submission_id must be nonblank'
        raise ValueError(msg)
    if not isinstance(request.text, str) or not request.text.strip():
        msg = 'Probe text must be nonblank'
        raise ValueError(msg)
    if store.get(request.submission_id) is not None:
        msg = 'Submission identity already exists; do not create or submit again'
        raise ValueError(msg)


async def create_and_submit_once(
    rpc: SessionRPC,
    store: AdmissionStore,
    request: DisposableAdmissionRequest,
) -> DisposableAdmissionResult:
    """Create, verify attachment, then submit exactly once using *rpc*.

    The session-creation file is an exclusive no-retry barrier. After a successful
    creation, the admission ledger is the durable no-retry barrier for submission.
    No cleanup RPC or compensating cancellation is attempted on failure.
    """
    _validate(request, store)
    prepared = await prepare_disposable_session(rpc, request.creation)
    admission_request = LiveAdmissionRequest(
        submission=Submission(session_id=prepared.session_id, submission_id=request.submission_id),
        text=request.text,
    )
    admission = await submit_once(store, admission_request, rpc=rpc)
    return DisposableAdmissionResult(prepared=prepared, admission=admission)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'DisposableAdmissionRequest',
    'DisposableAdmissionResult',
    'LiveAdmissionRequest',
    'Submission',
    '_validate',
    'create_and_submit_once',
    'dataclass',
    'prepare_disposable_session',
    'submit_once',
]
