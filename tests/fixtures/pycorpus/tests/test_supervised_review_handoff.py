"""Offline evidence-bound gateway reviewer handoff tests."""

import asyncio
import shutil
import subprocess
from pathlib import Path  # noqa: TC003
from typing import TYPE_CHECKING

import pytest

from hermes_harness.agents.supervised_agents import (
    SupervisedPolicy,
    prepare_supervised_plan,
)
from hermes_harness.agents.supervised_review_handoff import (
    HandoffSources,
    ReviewerHandoffError,
    prepare_reviewer_handoff_once,
    verify_reviewer_handoff,
)
from hermes_harness.core.delegation import Brief, Finding
from hermes_harness.gateway.admission_store import AdmissionStore
from hermes_harness.gateway.gateway_identity import Receipt, Submission
from hermes_harness.gateway.supervised_gateway_receipts import RoleAdmissions
from hermes_harness.git.worktree_delegation import (
    TaskWorkspace,
    delegate_in_worktree,
    prepare_worktree,
)

if TYPE_CHECKING:
    from hermes_harness.agents.supervised_agents import (
        SupervisedPlan,
    )


class Worker:
    def __init__(self, workspace: Path) -> None:
        self.workspace = workspace

    def run(self, brief: Brief, prior: Finding | None) -> Finding:
        assert prior is None
        target = self.workspace / 'src/safe.py'
        target.parent.mkdir(parents=True)
        target.write_text('written\n')
        return Finding(brief.worker_id, 'implemented')


class Gateway:
    def __init__(self, *, outcome: str = 'completed') -> None:
        self.outcome = outcome
        self.calls: list[str] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append(method)
        return {
            'admission_id': params['admission_id'],
            'ref': {'session_id': params['session_id']},
            'status': 'terminal',
            'outcome': self.outcome,
            'execution_generation': 1,
            'result': {
                'completed': self.outcome == 'completed',
                'final_response': 'worker finished',
            },
        }


@pytest.fixture
def setup(
    tmp_path: Path,
) -> tuple[SupervisedPlan, TaskWorkspace, RoleAdmissions, AdmissionStore, Gateway]:
    repo = tmp_path / 'repo'
    repo.mkdir()
    git = shutil.which('git')
    if git is None:
        pytest.fail('Git executable missing')
    for args in (
        ('init',),
        ('config', 'user.email', 'offline@example.invalid'),
        ('config', 'user.name', 'Offline'),
    ):
        subprocess.run([git, '-C', str(repo), *args], check=True, capture_output=True)  # noqa: S603
    (repo / 'README.md').write_text('baseline\n')
    for args in (('add', 'README.md'), ('commit', '-m', 'baseline')):
        subprocess.run([git, '-C', str(repo), *args], check=True, capture_output=True)  # noqa: S603
    root = tmp_path / 'state'
    root.mkdir()
    brief = Brief('gateway-handoff', 'Offline task', ('src/safe.py',), 'worker', 'reviewer')
    task = TaskWorkspace(root, tmp_path / 'worktree', brief)
    prepare_worktree(task, repo)
    delegate_in_worktree(task, Worker(task.workspace))
    plan = prepare_supervised_plan(
        brief,
        SupervisedPolicy(authorized_task_id=brief.task_id, authorized_workspace=task.workspace),
    )
    ids = RoleAdmissions(
        task_id=brief.task_id,
        worker_submission_id='worker-sub',
        reviewer_submission_id='review-sub',
    )
    store = AdmissionStore(tmp_path / 'admissions.sqlite')
    store.prepare(Submission('worker-session', 'worker-sub'))
    store.record_receipt('worker-sub', Receipt('worker-admission', 'queued'))
    return plan, task, ids, store, Gateway()


def test_successful_handoff_binds_audit_and_never_submits(
    setup: tuple[SupervisedPlan, TaskWorkspace, RoleAdmissions, AdmissionStore, Gateway],
) -> None:
    plan, task, ids, store, gateway = setup
    handoff = asyncio.run(
        prepare_reviewer_handoff_once(plan, task, HandoffSources(ids=ids, store=store, rpc=gateway))
    )
    assert handoff.audit_sha256 in handoff.reviewer_prompt
    assert 'worker-admission' in handoff.reviewer_prompt
    assert gateway.calls == ['prompt.receipt']
    verify_reviewer_handoff(task, handoff)


def test_changed_worker_file_invalidates_handoff(
    setup: tuple[SupervisedPlan, TaskWorkspace, RoleAdmissions, AdmissionStore, Gateway],
) -> None:
    plan, task, ids, store, gateway = setup
    handoff = asyncio.run(
        prepare_reviewer_handoff_once(plan, task, HandoffSources(ids=ids, store=store, rpc=gateway))
    )
    (task.workspace / 'src/safe.py').write_text('tampered\n')
    with pytest.raises(Exception, match='evidence changed'):
        verify_reviewer_handoff(task, handoff)


def test_failed_worker_never_prepares_reviewer(
    setup: tuple[SupervisedPlan, TaskWorkspace, RoleAdmissions, AdmissionStore, Gateway],
) -> None:
    plan, task, ids, store, _gateway = setup
    gateway = Gateway(outcome='failed')
    with pytest.raises(ReviewerHandoffError, match='successful'):
        asyncio.run(
            prepare_reviewer_handoff_once(
                plan, task, HandoffSources(ids=ids, store=store, rpc=gateway)
            )
        )
    assert gateway.calls == ['prompt.receipt']


def test_preexisting_reviewer_submission_blocks_without_rpc(
    setup: tuple[SupervisedPlan, TaskWorkspace, RoleAdmissions, AdmissionStore, Gateway],
) -> None:
    plan, task, ids, store, gateway = setup
    store.prepare(Submission('review-session', 'review-sub'))
    with pytest.raises(ReviewerHandoffError, match='already submitted'):
        asyncio.run(
            prepare_reviewer_handoff_once(
                plan, task, HandoffSources(ids=ids, store=store, rpc=gateway)
            )
        )
    assert not gateway.calls


def test_task_identity_mismatch_blocks_without_rpc(
    setup: tuple[SupervisedPlan, TaskWorkspace, RoleAdmissions, AdmissionStore, Gateway],
) -> None:
    plan, task, _ids, store, gateway = setup
    wrong = RoleAdmissions(
        task_id='wrong', worker_submission_id='worker-sub', reviewer_submission_id='review-sub'
    )
    with pytest.raises(ReviewerHandoffError, match='task identity'):
        asyncio.run(
            prepare_reviewer_handoff_once(
                plan, task, HandoffSources(ids=wrong, store=store, rpc=gateway)
            )
        )
    assert not gateway.calls
