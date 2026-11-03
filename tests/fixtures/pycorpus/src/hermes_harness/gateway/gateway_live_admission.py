"""Single-submit live admission probe with durable fail-closed identity tracking."""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

from hermes_harness.gateway.admission_store import LocalState
from hermes_harness.gateway.gateway_identity import (
    GatewayContractError,
    GatewayOutcomeUnknownError,
    parse_receipt,
)

if TYPE_CHECKING:
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.gateway.gateway_identity import (
        Receipt,
        Submission,
    )


class AsyncGatewayRPC(Protocol):
    """Minimal asynchronous RPC surface supplied by an attached Hermes gateway."""

    async def rpc(self, method: str, **params: object) -> object: ...


@dataclass(frozen=True, slots=True)
class LiveAdmissionRequest:
    """One immutable request body and client-generated submission identity."""

    submission: Submission
    text: str


@dataclass(frozen=True, slots=True)
class LiveAdmissionResult:
    """Receipt identities verified during one live admission probe."""

    admission_id: str
    submit_status: str
    observed_status: str


def _validate_request(request: LiveAdmissionRequest) -> None:
    if not isinstance(request.text, str) or not request.text.strip():
        msg = 'Probe text must be a nonblank string'
        raise ValueError(msg)
    if request.submission.admission_id is not None:
        msg = 'Live probe requires a submission without an admission ID'
        raise ValueError(msg)


async def _submit_and_parse(
    rpc: AsyncGatewayRPC,
    request: LiveAdmissionRequest,
) -> Receipt:
    try:
        raw_submit = await rpc.rpc(
            'prompt.submit',
            session_id=request.submission.session_id,
            submission_id=request.submission.submission_id,
            text=request.text,
        )
    except Exception as exc:
        msg = 'prompt.submit outcome unknown; do not automatically retry'
        raise GatewayOutcomeUnknownError(msg) from exc
    try:
        return parse_receipt(raw_submit)
    except GatewayContractError as exc:
        msg = 'prompt.submit returned no verifiable admission identity; do not retry'
        raise GatewayOutcomeUnknownError(msg) from exc


async def _read_matching_receipt(
    rpc: AsyncGatewayRPC,
    request: LiveAdmissionRequest,
    accepted: Receipt,
) -> Receipt:
    raw_observed = await rpc.rpc(
        'prompt.receipt',
        session_id=request.submission.session_id,
        admission_id=accepted.admission_id,
    )
    return parse_receipt(raw_observed, expected_admission_id=accepted.admission_id)


async def submit_once(
    store: AdmissionStore,
    request: LiveAdmissionRequest,
    *,
    rpc: AsyncGatewayRPC,
) -> LiveAdmissionResult:
    """Submit once, persist the first identity, and verify it with one receipt read.

    The durable record is created before the only submit RPC. A transport failure
    or malformed submit response leaves the outcome unknown and is never retried.
    Once the first receipt is valid, its admission ID is persisted before the
    follow-up read, so a failed receipt read cannot erase known identity.
    """
    _validate_request(request)
    submission = request.submission
    store.prepare(submission)
    store.mark_outcome_unknown(submission.submission_id)

    accepted = await _submit_and_parse(rpc, request)
    store.record_receipt(submission.submission_id, accepted)
    observed = await _read_matching_receipt(rpc, request, accepted)
    store.record_receipt(submission.submission_id, observed)

    record = store.get(submission.submission_id)
    if record is None or record.local_state is not LocalState.ADMITTED:
        msg = 'Verified receipt was not durably recorded'
        raise RuntimeError(msg)

    return LiveAdmissionResult(
        admission_id=accepted.admission_id,
        submit_status=accepted.status,
        observed_status=observed.status,
    )


# Temporary exports for legacy module import compatibility.
__all__ = [
    'AsyncGatewayRPC',
    'GatewayContractError',
    'GatewayOutcomeUnknownError',
    'LiveAdmissionRequest',
    'LiveAdmissionResult',
    'LocalState',
    'Protocol',
    '_read_matching_receipt',
    '_submit_and_parse',
    '_validate_request',
    'dataclass',
    'parse_receipt',
    'submit_once',
]
