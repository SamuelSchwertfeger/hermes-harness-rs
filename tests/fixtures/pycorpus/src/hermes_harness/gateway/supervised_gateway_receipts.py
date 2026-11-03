"""Reconcile gateway-owned worker/reviewer terminal evidence without dispatch.

This bridge accepts existing durable admission identities only. It cannot create a
session, submit prompts, invoke agents, grant approval, or mutate a worktree.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.gateway.gateway_terminal_reconciliation import (
    reconcile_terminal_once,
)

if TYPE_CHECKING:
    from hermes_harness.agents.supervised_agents import SupervisedPlan
    from hermes_harness.gateway.admission_store import AdmissionStore
    from hermes_harness.gateway.gateway_terminal_reconciliation import (
        ReceiptReader,
        TerminalEvidence,
    )

MAX_ROLE_RESPONSE_CHARS = 40_000


class RoleReceiptError(RuntimeError):
    """Missing, mismatched, or unsuccessful gateway role evidence."""


@dataclass(frozen=True, slots=True, kw_only=True)
class RoleAdmissions:
    """Operator-bound immutable identities of two previously admitted turns."""

    task_id: str
    worker_submission_id: str
    reviewer_submission_id: str


@dataclass(frozen=True, slots=True)
class ReceiptSources:
    """Existing ledger and read-only terminal receipt transport."""

    store: AdmissionStore
    rpc: ReceiptReader


@dataclass(frozen=True, slots=True)
class RoleCompletion:
    """Successful receipts, not authority to approve or mutate Git."""

    worker: TerminalEvidence
    reviewer: TerminalEvidence


def _validate(plan: SupervisedPlan, ids: RoleAdmissions, store: AdmissionStore) -> None:
    if not ids.task_id or ids.task_id != plan.task_id:
        msg = 'Admission binding must match the authorized task'
        raise RoleReceiptError(msg)
    if (
        not ids.worker_submission_id
        or not ids.reviewer_submission_id
        or ids.worker_submission_id == ids.reviewer_submission_id
    ):
        msg = 'Worker and reviewer require distinct nonblank submission IDs'
        raise RoleReceiptError(msg)
    worker = store.get(ids.worker_submission_id)
    reviewer = store.get(ids.reviewer_submission_id)
    if worker is None or reviewer is None:
        msg = 'Both roles require preexisting durable admissions'
        raise RoleReceiptError(msg)
    if not worker.admission_id or not reviewer.admission_id:
        msg = 'Both roles require known gateway admission IDs; never resubmit'
        raise RoleReceiptError(msg)
    if worker.admission_id == reviewer.admission_id or worker.session_id == reviewer.session_id:
        msg = 'Worker and reviewer must use distinct gateway admissions and sessions'
        raise RoleReceiptError(msg)


def _successful(evidence: TerminalEvidence, role: str) -> None:
    if evidence.outcome != 'completed' or not evidence.final_response:
        msg = f'{role} did not provide a successful nonempty terminal response'
        raise RoleReceiptError(msg)
    if len(evidence.final_response) > MAX_ROLE_RESPONSE_CHARS:
        msg = f'{role} terminal response exceeds the bounded evidence limit'
        raise RoleReceiptError(msg)


async def reconcile_supervised_roles_once(
    plan: SupervisedPlan,
    ids: RoleAdmissions,
    *,
    sources: ReceiptSources,
) -> RoleCompletion:
    """Verify worker then reviewer receipt, without submitting any prompts.

    Reviewer receipt is read only after the worker is terminal-successful. A
    receipt failure preserves whatever the admission store already knows.
    This is not proof that the reviewer actually read the worker's Git audit:
    that binding must be established by a later execution adapter.
    """
    _validate(plan, ids, sources.store)
    worker = await reconcile_terminal_once(sources.rpc, sources.store, ids.worker_submission_id)
    _successful(worker, 'Worker')
    reviewer = await reconcile_terminal_once(sources.rpc, sources.store, ids.reviewer_submission_id)
    _successful(reviewer, 'Reviewer')
    return RoleCompletion(worker, reviewer)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_ROLE_RESPONSE_CHARS',
    'ReceiptSources',
    'RoleAdmissions',
    'RoleCompletion',
    'RoleReceiptError',
    '_successful',
    '_validate',
    'dataclass',
    'reconcile_supervised_roles_once',
    'reconcile_terminal_once',
]
