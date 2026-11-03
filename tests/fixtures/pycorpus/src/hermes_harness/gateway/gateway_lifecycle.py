"""Fail-closed, simulation-only Hermes admission lifecycle tracking.

The real gateway RPC schema has not been verified. No production connection is
constructed here; an injected RPC seam is accepted only with explicit opt-in.
"""

import json
from dataclasses import asdict, dataclass
from enum import StrEnum
from typing import TYPE_CHECKING, Protocol
from uuid import uuid4

if TYPE_CHECKING:
    from pathlib import Path


class AdmissionState(StrEnum):
    NEW = 'new'
    SUBMIT_UNKNOWN = 'submit_unknown'
    RECEIPT_RECORDED = 'receipt_recorded'
    CANCEL_REQUESTED = 'cancel_requested'
    CANCEL_UNKNOWN = 'cancel_unknown'
    TERMINAL_UNVERIFIED = 'terminal_unverified'


class AdmissionError(RuntimeError):
    """Reject untrusted lifecycle operations or inconsistent state."""


class GatewayRPC(Protocol):
    """Injected RPC transport; use a scripted fake until the API is verified."""

    def call(self, method: str, params: dict[str, str]) -> dict[str, object]:
        """Invoke a scripted method and return a parsed response."""
        ...


@dataclass(frozen=True, slots=True)
class Admission:
    session_id: str
    submission_id: str
    state: AdmissionState
    receipt: dict[str, object] | None = None


@dataclass(frozen=True, slots=True)
class LifecycleConfig:
    evidence_root: Path
    simulated_only: bool = True


class GatewayLifecycle:
    """Persist decisions without claiming that remote cancellation stopped work."""

    def __init__(self, rpc: GatewayRPC, config: LifecycleConfig) -> None:
        self._rpc = rpc
        self._config = config
        if not config.simulated_only:
            msg = 'Live gateway transport is not verified or enabled'
            raise AdmissionError(msg)

    def _save(self, admission: Admission) -> Admission:
        root = self._config.evidence_root
        root.mkdir(parents=True, exist_ok=True, mode=0o700)
        payload = json.dumps(asdict(admission), sort_keys=True).encode('utf-8')
        path = root / f'{admission.submission_id}.json'
        if path.exists():
            path.write_bytes(payload)
        else:
            # Exclusive creation prevents silent replacement of an unrelated admission.
            with path.open('xb') as stream:
                stream.write(payload)
        path.chmod(0o600)
        return admission

    def new(self, session_id: str) -> Admission:
        if not session_id.strip():
            msg = 'A stable, nonblank session ID is required'
            raise AdmissionError(msg)
        return self._save(Admission(session_id, uuid4().hex, AdmissionState.NEW))

    def submit(self, admission: Admission, prompt: str) -> Admission:
        """Simulate submission; an exception always leaves the outcome unknown."""
        if admission.state != AdmissionState.NEW or not prompt.strip():
            msg = 'Only a fresh admission with a nonblank prompt may be submitted'
            raise AdmissionError(msg)
        uncertain = self._save(
            Admission(admission.session_id, admission.submission_id, AdmissionState.SUBMIT_UNKNOWN)
        )
        try:
            response = self._rpc.call(
                'prompt.submit',
                {
                    'session_id': uncertain.session_id,
                    'submission_id': uncertain.submission_id,
                    'prompt': prompt,
                },
            )
        except OSError:
            return uncertain
        return self._record(uncertain, response)

    def _record(self, admission: Admission, response: dict[str, object]) -> Admission:
        if not isinstance(response, dict):
            msg = 'RPC must return a JSON object'
            raise AdmissionError(msg)
        # A receipt is not proof of completed execution.
        return self._save(
            Admission(
                admission.session_id,
                admission.submission_id,
                AdmissionState.RECEIPT_RECORDED,
                response,
            )
        )

    def receipt(self, admission: Admission) -> Admission:
        """Reconcile the same ID; never retry a potentially accepted submission."""
        if admission.state not in {
            AdmissionState.SUBMIT_UNKNOWN,
            AdmissionState.RECEIPT_RECORDED,
            AdmissionState.CANCEL_UNKNOWN,
            AdmissionState.CANCEL_REQUESTED,
        }:
            msg = 'Receipt lookup is not valid for this admission state'
            raise AdmissionError(msg)
        try:
            response = self._rpc.call(
                'prompt.receipt',
                {
                    'session_id': admission.session_id,
                    'submission_id': admission.submission_id,
                },
            )
        except OSError:
            return admission
        return self._record(admission, response)

    def cancel(self, admission: Admission, admission_id: str) -> Admission:
        """Request cancellation without treating acceptance as termination proof."""
        if not admission_id.strip() or admission.state not in {
            AdmissionState.RECEIPT_RECORDED,
            AdmissionState.CANCEL_UNKNOWN,
        }:
            msg = 'Cancellation requires a known receipt and explicit admission ID'
            raise AdmissionError(msg)
        pending = self._save(
            Admission(
                admission.session_id,
                admission.submission_id,
                AdmissionState.CANCEL_UNKNOWN,
                admission.receipt,
            )
        )
        try:
            response = self._rpc.call(
                'prompt.cancel',
                {
                    'session_id': pending.session_id,
                    'admission_id': admission_id,
                },
            )
        except OSError:
            return pending
        if not isinstance(response, dict):
            return pending
        return self._save(
            Admission(
                pending.session_id,
                pending.submission_id,
                AdmissionState.CANCEL_REQUESTED,
                response,
            )
        )


# Temporary exports for legacy module import compatibility.
__all__ = [
    'Admission',
    'AdmissionError',
    'AdmissionState',
    'GatewayLifecycle',
    'GatewayRPC',
    'LifecycleConfig',
    'Protocol',
    'StrEnum',
    'asdict',
    'dataclass',
    'json',
    'uuid4',
]
