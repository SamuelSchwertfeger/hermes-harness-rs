"""Fail-closed Hermes admission identity and receipt validation (offline only).

This module never connects to the gateway. The caller supplies a scripted RPC
implementation; authenticated live transport is a separate, gated milestone.
"""

from dataclasses import dataclass
from typing import Protocol
from uuid import uuid4


class GatewayContractError(ValueError):
    """Remote response violates the verified identity contract."""


class GatewayOutcomeUnknownError(RuntimeError):
    """Transport failed; a request may still have reached its owner."""


class RPC(Protocol):
    """Minimal injectable synchronous RPC seam for offline contract tests."""

    def call(self, method: str, params: dict[str, str]) -> object:
        """Return an untrusted decoded RPC result."""
        ...


@dataclass(frozen=True, slots=True)
class Receipt:
    """Identity and opaque status returned by Hermes (not termination evidence)."""

    admission_id: str
    status: str
    execution_generation: int | None = None


@dataclass(frozen=True, slots=True)
class Submission:
    """Client-generated request identity, distinct from the gateway admission ID."""

    session_id: str
    submission_id: str
    admission_id: str | None = None


def _nonblank(value: object, key: str) -> str:
    if not isinstance(value, str) or not value.strip():
        msg = f'Gateway {key} must be a nonblank string'
        raise GatewayContractError(msg)
    return value


def parse_receipt(raw: object, *, expected_admission_id: str | None = None) -> Receipt:
    """Validate identity without speculating about Hermes terminal status values."""
    if not isinstance(raw, dict):
        msg = 'Gateway receipt must be a JSON object'
        raise GatewayContractError(msg)
    admission_id = _nonblank(raw.get('admission_id'), 'admission_id')
    status = _nonblank(raw.get('status'), 'status')
    if expected_admission_id is not None and admission_id != expected_admission_id:
        msg = 'Gateway receipt changed admission identity'
        raise GatewayContractError(msg)
    generation = raw.get('execution_generation')
    if generation is not None and (type(generation) is not int or generation < 0):
        msg = 'Gateway execution_generation must be a nonnegative integer'
        raise GatewayContractError(msg)
    return Receipt(admission_id, status, generation)


class OfflineAdmissionClient:
    """Validate scripted Hermes calls; never retries on communication failure."""

    def __init__(self, rpc: RPC) -> None:
        self._rpc = rpc

    @staticmethod
    def new(session_id: str) -> Submission:
        return Submission(_nonblank(session_id, 'session_id'), uuid4().hex)

    def _call(self, method: str, params: dict[str, str]) -> object:
        try:
            return self._rpc.call(method, params)
        except OSError as exc:
            msg = f'{method} outcome unknown; do not automatically retry'
            raise GatewayOutcomeUnknownError(msg) from exc

    def submit(self, submission: Submission, text: str) -> tuple[Submission, Receipt]:
        """Send once, using Hermes's `text` field and stable `submission_id`."""
        if submission.admission_id is not None:
            msg = 'Submission already has a gateway admission identity'
            raise GatewayContractError(msg)
        response = self._call(
            'prompt.submit',
            {
                'session_id': submission.session_id,
                'submission_id': submission.submission_id,
                'text': _nonblank(text, 'text'),
            },
        )
        receipt = parse_receipt(response)
        return Submission(
            submission.session_id, submission.submission_id, receipt.admission_id
        ), receipt

    def receipt(self, submission: Submission) -> Receipt:
        admission_id = _nonblank(submission.admission_id, 'admission_id')
        return parse_receipt(
            self._call(
                'prompt.receipt',
                {
                    'session_id': submission.session_id,
                    'admission_id': admission_id,
                },
            ),
            expected_admission_id=admission_id,
        )

    def cancel_queued(self, submission: Submission) -> Receipt:
        """Request queued cancellation; response alone does not prove execution stopped."""
        admission_id = _nonblank(submission.admission_id, 'admission_id')
        return parse_receipt(
            self._call(
                'prompt.cancel',
                {
                    'session_id': submission.session_id,
                    'admission_id': admission_id,
                },
            ),
            expected_admission_id=admission_id,
        )


# Temporary exports for legacy module import compatibility.
__all__ = [
    'RPC',
    'GatewayContractError',
    'GatewayOutcomeUnknownError',
    'OfflineAdmissionClient',
    'Protocol',
    'Receipt',
    'Submission',
    '_nonblank',
    'dataclass',
    'parse_receipt',
    'uuid4',
]
