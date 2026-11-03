"""Operator-gated adapter binding socket capabilities to the M3 admission ledger.

No gateway transport is constructed here. A trusted operator must inject an
independently qualified authenticated RPC; provider calls remain disabled by default.
"""

import asyncio
import hashlib
import hmac
import sqlite3
from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.broker.live_capability_broker import SHA256_HEX_LENGTH
from hermes_harness.gateway.gateway_identity import Submission
from hermes_harness.gateway.gateway_live_admission import LiveAdmissionRequest, submit_once

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.broker.live_capability_broker import BoundRequest
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.gateway.gateway_live_admission import AsyncGatewayRPC
    from hermes_harness.services.live_service_controller import ServiceController
    from hermes_harness.supervised.live_execution_binding import ExecutionBindingStore


class OperatorAuthorizationError(RuntimeError):
    """No matching, unconsumed human authorization exists."""


@dataclass(frozen=True, slots=True)
class OperatorGrant:
    """Explicit authorization, bound to exact request and running service."""

    task_id: str
    role: str
    session_id: str
    submission_id: str
    unit: str
    text_sha256: str


class OperatorGrantStore:
    """One-time role grants; grants never authorize gateway credentials or commands."""

    def __init__(self, path: Path) -> None:
        if path.is_symlink():
            msg = 'Grant database symlink rejected'
            raise OperatorAuthorizationError(msg)
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.path = path
        with self._connect() as db:
            db.execute(
                'CREATE TABLE IF NOT EXISTS grants ('
                'submission_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, '
                'role TEXT NOT NULL, session_id TEXT NOT NULL, unit TEXT NOT NULL, '
                'text_sha256 TEXT NOT NULL, consumed INTEGER NOT NULL DEFAULT 0)'
            )
        path.chmod(0o600)

    def _connect(self) -> sqlite3.Connection:
        return sqlite3.connect(self.path, timeout=5)

    def authorize(self, grant: OperatorGrant, *, human_confirmed: bool = False) -> None:
        """Only trusted operator control-plane code should ever invoke this method."""
        if not human_confirmed:
            msg = 'Explicit human confirmation required'
            raise OperatorAuthorizationError(msg)
        if (
            grant.role not in ('worker', 'reviewer')
            or not all((
                grant.task_id,
                grant.session_id,
                grant.submission_id,
                grant.unit,
            ))
            or len(grant.text_sha256) != SHA256_HEX_LENGTH
        ):
            msg = 'Invalid authorization binding'
            raise OperatorAuthorizationError(msg)
        with self._connect() as db:
            db.execute(
                'INSERT INTO grants VALUES (?, ?, ?, ?, ?, ?, 0)',
                (
                    grant.submission_id,
                    grant.task_id,
                    grant.role,
                    grant.session_id,
                    grant.unit,
                    grant.text_sha256,
                ),
            )

    def consume(self, grant: OperatorGrant) -> None:
        """Atomically fence the authorization before attempting gateway admission."""
        with self._connect() as db:
            row = db.execute(
                'UPDATE grants SET consumed = 1 '
                'WHERE submission_id = ? AND task_id = ? AND role = ? '
                'AND session_id = ? AND unit = ? AND text_sha256 = ? '
                'AND consumed = 0',
                (
                    grant.submission_id,
                    grant.task_id,
                    grant.role,
                    grant.session_id,
                    grant.unit,
                    grant.text_sha256,
                ),
            )
            if row.rowcount != 1:
                msg = 'No matching unconsumed operator grant'
                raise OperatorAuthorizationError(msg)


@dataclass(slots=True)
class QualifiedAdmissionAdapter:
    """Host-only M3 adapter; bind capability, service and grant before RPC.

    Run on a dedicated broker thread without an active asyncio loop. If the M3
    ledger already contains a submission, never send it again, even after a crash.
    """

    grants: OperatorGrantStore
    bindings: ExecutionBindingStore
    services: ServiceController
    admissions: AdmissionStore
    rpc: AsyncGatewayRPC | None = None

    def invoke(self, request: BoundRequest, text: str) -> dict[str, object]:
        if self.rpc is None:
            msg = 'Authenticated Hermes RPC not qualified'
            raise OperatorAuthorizationError(msg)
        binding = self.bindings.get(request.task_id, request.role)  # type: ignore[arg-type]
        if binding is None or (
            binding.session_id != request.session_id
            or binding.submission_id != request.submission_id
        ):
            msg = 'Missing service/submission binding'
            raise OperatorAuthorizationError(msg)
        self.bindings.require_running(binding, self.services.journal)
        if not self.services.transport.is_active(binding.unit):
            msg = 'Stale or stopped service'
            raise OperatorAuthorizationError(msg)
        digest = hashlib.sha256(text.encode()).hexdigest()
        if not hmac.compare_digest(digest, request.text_sha256):
            msg = 'Prompt mismatch'
            raise OperatorAuthorizationError(msg)
        if self.admissions.get(request.submission_id) is not None:
            msg = 'M3 submission already recorded or uncertain'
            raise OperatorAuthorizationError(msg)
        self.grants.consume(
            OperatorGrant(
                request.task_id,
                request.role,
                request.session_id,
                request.submission_id,
                binding.unit,
                digest,
            )
        )
        result = asyncio.run(
            submit_once(
                self.admissions,
                LiveAdmissionRequest(Submission(request.session_id, request.submission_id), text),
                rpc=self.rpc,
            )
        )
        return {'admission_id': result.admission_id}


# Temporary exports for legacy module import compatibility.
__all__ = [
    'SHA256_HEX_LENGTH',
    'LiveAdmissionRequest',
    'OperatorAuthorizationError',
    'OperatorGrant',
    'OperatorGrantStore',
    'QualifiedAdmissionAdapter',
    'Submission',
    'asyncio',
    'dataclass',
    'hashlib',
    'hmac',
    'sqlite3',
    'submit_once',
]
