"""Offline gateway role receipt bridge integration tests."""

import asyncio
from typing import TYPE_CHECKING

import pytest

from hermes_harness.agents.supervised_agents import SupervisedPolicy, prepare_supervised_plan
from hermes_harness.core.delegation import Brief
from hermes_harness.gateway.admission_store import AdmissionStore
from hermes_harness.gateway.gateway_identity import Receipt, Submission
from hermes_harness.gateway.supervised_gateway_receipts import (
    ReceiptSources,
    RoleAdmissions,
    RoleReceiptError,
    reconcile_supervised_roles_once,
)

if TYPE_CHECKING:
    from pathlib import Path


class ScriptedReceipts:
    def __init__(self, responses: dict[str, object]) -> None:
        self.responses = responses
        self.calls: list[tuple[str, dict[str, object]]] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        result = self.responses[str(params['admission_id'])]
        if isinstance(result, Exception):
            raise result
        return result


def _receipt(session: str, admission: str, *, outcome: str = 'completed') -> dict[str, object]:
    return {
        'admission_id': admission,
        'ref': {'session_id': session},
        'status': 'terminal',
        'outcome': outcome,
        'execution_generation': 1,
        'result': {'completed': outcome == 'completed', 'final_response': 'offline test evidence'},
    }


def setup(tmp_path: Path) -> tuple[object, RoleAdmissions, AdmissionStore, ScriptedReceipts]:
    tmp_path.joinpath('workspace').mkdir()
    brief = Brief('task1', 'test', ('file.txt',), 'worker', 'reviewer')
    plan = prepare_supervised_plan(
        brief,
        SupervisedPolicy(authorized_task_id='task1', authorized_workspace=tmp_path / 'workspace'),
    )
    ids = RoleAdmissions(
        task_id='task1', worker_submission_id='sub-w', reviewer_submission_id='sub-r'
    )
    store = AdmissionStore(tmp_path / 'admissions.sqlite')
    for role in ('w', 'r'):
        store.prepare(Submission(f'session-{role}', f'sub-{role}'))
        store.record_receipt(f'sub-{role}', Receipt(f'admission-{role}', 'queued'))
    rpc = ScriptedReceipts({
        'admission-w': _receipt('session-w', 'admission-w'),
        'admission-r': _receipt('session-r', 'admission-r'),
    })
    return plan, ids, store, rpc


def test_two_distinct_successes_only_read_receipts(tmp_path: Path) -> None:
    plan, ids, store, rpc = setup(tmp_path)
    complete = asyncio.run(
        reconcile_supervised_roles_once(plan, ids, sources=ReceiptSources(store=store, rpc=rpc))
    )
    assert complete.worker.outcome == complete.reviewer.outcome == 'completed'
    assert [name for name, _ in rpc.calls] == ['prompt.receipt', 'prompt.receipt']
    assert [params['admission_id'] for _, params in rpc.calls] == ['admission-w', 'admission-r']


def test_rejects_wrong_task_without_rpc(tmp_path: Path) -> None:
    plan, ids, store, rpc = setup(tmp_path)
    ids = RoleAdmissions(
        task_id='other',
        worker_submission_id=ids.worker_submission_id,
        reviewer_submission_id=ids.reviewer_submission_id,
    )
    with pytest.raises(RoleReceiptError, match='authorized task'):
        asyncio.run(
            reconcile_supervised_roles_once(plan, ids, sources=ReceiptSources(store=store, rpc=rpc))
        )
    assert not rpc.calls


def test_rejects_duplicate_role_id_without_rpc(tmp_path: Path) -> None:
    plan, ids, store, rpc = setup(tmp_path)
    ids = RoleAdmissions(
        task_id=ids.task_id, worker_submission_id='sub-w', reviewer_submission_id='sub-w'
    )
    with pytest.raises(RoleReceiptError, match='distinct'):
        asyncio.run(
            reconcile_supervised_roles_once(plan, ids, sources=ReceiptSources(store=store, rpc=rpc))
        )
    assert not rpc.calls


def test_failed_worker_never_reads_reviewer(tmp_path: Path) -> None:
    plan, ids, store, rpc = setup(tmp_path)
    rpc.responses['admission-w'] = _receipt('session-w', 'admission-w', outcome='failed')
    with pytest.raises(RoleReceiptError, match='Worker'):
        asyncio.run(
            reconcile_supervised_roles_once(plan, ids, sources=ReceiptSources(store=store, rpc=rpc))
        )
    assert [name for name, _ in rpc.calls] == ['prompt.receipt']


def test_unknown_worker_preserves_ledger_and_never_retries(tmp_path: Path) -> None:
    plan, ids, store, rpc = setup(tmp_path)
    before = store.get('sub-w')
    rpc.responses['admission-w'] = ConnectionError('unavailable')
    with pytest.raises(ConnectionError, match='unavailable'):
        asyncio.run(
            reconcile_supervised_roles_once(plan, ids, sources=ReceiptSources(store=store, rpc=rpc))
        )
    assert store.get('sub-w') == before
    assert len(rpc.calls) == 1


def test_missing_admission_rejects_before_rpc(tmp_path: Path) -> None:
    plan, ids, store, rpc = setup(tmp_path)
    ids = RoleAdmissions(
        task_id=ids.task_id, worker_submission_id='missing', reviewer_submission_id='sub-r'
    )
    with pytest.raises(RoleReceiptError, match='preexisting'):
        asyncio.run(
            reconcile_supervised_roles_once(plan, ids, sources=ReceiptSources(store=store, rpc=rpc))
        )
    assert not rpc.calls
