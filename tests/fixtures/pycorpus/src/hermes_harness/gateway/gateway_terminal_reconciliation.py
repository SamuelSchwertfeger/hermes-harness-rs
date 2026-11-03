"""Read-only terminal receipt reconciliation for a previously admitted prompt.

No RPC in this module can create a session, submit a prompt, cancel a turn,
or start a gateway. The owner remains the authority for execution outcomes.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol

from hermes_harness.gateway.gateway_identity import GatewayContractError, parse_receipt

if TYPE_CHECKING:
    from hermes_harness.gateway.admission_store import AdmissionRecord, AdmissionStore


class TerminalReconciliationError(RuntimeError):
    """The observed receipt does not prove a terminal result."""


class ReceiptReader(Protocol):
    async def rpc(self, method: str, **params: object) -> object: ...


@dataclass(frozen=True, slots=True)
class TerminalEvidence:
    """Sanitized terminal execution evidence; never stores a model response."""

    submission_id: str
    admission_id: str
    outcome: str
    execution_generation: int | None
    final_response: str | None
    recorded: AdmissionRecord


def _verify_outcome_result(
    outcome: str,
    result: object,
) -> str | None:
    if outcome == 'completed':
        if not isinstance(result, dict) or result.get('completed') is not True:
            msg = 'Completed outcome lacks committed completion evidence'
            raise TerminalReconciliationError(msg)
        if result.get('failed') is True or result.get('interrupted') is True:
            msg = 'Completed outcome conflicts with execution flags'
            raise TerminalReconciliationError(msg)
    elif isinstance(result, dict) and result.get('completed') is True:
        msg = 'Non-completed outcome conflicts with completion flag'
        raise TerminalReconciliationError(msg)

    response = result.get('final_response') if isinstance(result, dict) else None
    if response is not None and not isinstance(response, str):
        msg = 'Final response must be text or null'
        raise TerminalReconciliationError(msg)
    return response


def _verify_terminal(raw: object, expected_id: str) -> tuple[str, str | None]:
    if not isinstance(raw, dict):
        msg = 'Receipt must be an object'
        raise GatewayContractError(msg)

    if raw.get('admission_id') != expected_id or raw.get('status') != 'terminal':
        msg = 'Matching terminal receipt is required'
        raise TerminalReconciliationError(msg)

    outcome = raw.get('outcome')
    if outcome not in ('completed', 'failed', 'interrupted', 'cancelled'):
        msg = 'Terminal outcome must be explicit and recognized'
        raise TerminalReconciliationError(msg)

    response = _verify_outcome_result(outcome, raw.get('result'))
    return outcome, response


async def reconcile_terminal_once(
    rpc: ReceiptReader,
    store: AdmissionStore,
    submission_id: str,
) -> TerminalEvidence:
    """Read one authoritative receipt, then persist only verified terminal state.

    Never substitutes a submission lookup or retries an uncertain RPC. Missing
    admission identity is an explicit blocker, not permission to resubmit.
    """
    record = store.get(submission_id)
    if record is None or record.admission_id is None:
        msg = 'Known durable admission ID is required; never resubmit'
        raise TerminalReconciliationError(msg)
    raw = await rpc.rpc(
        'prompt.receipt',
        session_id=record.session_id,
        admission_id=record.admission_id,
        include_result=True,
    )
    receipt = parse_receipt(raw, expected_admission_id=record.admission_id)
    if (
        isinstance(raw, dict)
        and isinstance(raw.get('ref'), dict)
        and raw['ref'].get('session_id') != record.session_id
    ):
        msg = 'Receipt session identity changed'
        raise TerminalReconciliationError(msg)
    outcome, final_response = _verify_terminal(raw, record.admission_id)
    updated = store.record_receipt(submission_id, receipt, reconciled=True)
    return TerminalEvidence(
        submission_id=submission_id,
        admission_id=record.admission_id,
        outcome=outcome,
        execution_generation=receipt.execution_generation,
        final_response=final_response,
        recorded=updated,
    )


# Temporary exports for legacy module import compatibility.
__all__ = [
    'GatewayContractError',
    'Protocol',
    'ReceiptReader',
    'TerminalEvidence',
    'TerminalReconciliationError',
    '_verify_outcome_result',
    '_verify_terminal',
    'dataclass',
    'parse_receipt',
    'reconcile_terminal_once',
]
