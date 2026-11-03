"""Offline tests for bounded worktree delegation using disposable Git repos."""

import shutil
import subprocess
from pathlib import Path  # noqa: TC003 - runtime annotations on Python 3.13

import pytest

from hermes_harness.core.delegation import Brief, DelegationError, Finding, Phase, phase
from hermes_harness.git.worktree_delegation import (
    TaskWorkspace,
    delegate_in_worktree,
    prepare_worktree,
    request_worktree_approval,
    review_worktree,
    verified_audit,
)
from hermes_harness.git.worktrees import WorktreeError
from hermes_harness.supervised.offline_orchestrator import (
    OfflineAgents,
    OrchestrationState,
    inspect_offline_task,
    run_offline_task,
)


class FakeAgent:
    def __init__(self, identity: str, workspace: Path, *, filename: str | None = None) -> None:
        self.identity = identity
        self.workspace = workspace
        self.filename = filename
        self.calls = 0

    def run(self, _brief: Brief, prior: Finding | None) -> Finding:
        self.calls += 1
        if self.filename is not None:
            target = self.workspace / self.filename
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text('offline fixture\n')
        return Finding(self.identity, 'reviewed' if prior else 'implemented')


def _git(repo: Path, *args: str) -> None:
    git_executable = shutil.which('git')
    if git_executable is None:
        pytest.fail('Git executable is required for worktree tests')
    subprocess.run(  # noqa: S603 - disposable Git test repo; arguments are fixed by tests
        [git_executable, '-C', str(repo), *args],
        check=True,
        capture_output=True,
        timeout=15,
    )


@pytest.fixture
def setup(tmp_path: Path) -> tuple[Path, Path, Path, Brief]:
    repo = tmp_path / 'repo'
    repo.mkdir()
    _git(repo, 'init')
    _git(repo, 'config', 'user.email', 'offline@example.invalid')
    _git(repo, 'config', 'user.name', 'Offline')
    (repo / 'README.md').write_text('fixture\n')
    _git(repo, 'add', 'README.md')
    _git(repo, 'commit', '-m', 'baseline')
    root = tmp_path / 'evidence'
    root.mkdir()
    workspace = tmp_path / 'worker-worktree'
    brief = Brief('ticket-one', 'Offline task', ('src/safe.py',), 'worker', 'reviewer')
    return root, repo, workspace, brief


def test_complete_audited_workflow(setup: tuple[Path, Path, Path, Brief]) -> None:
    root, repo, workspace, brief = setup
    prepare_worktree(TaskWorkspace(root, workspace, brief), repo)
    delegate_in_worktree(
        TaskWorkspace(root, workspace, brief),
        FakeAgent('worker', workspace, filename='src/safe.py'),
    )
    audit = verified_audit(TaskWorkspace(root, workspace, brief))
    assert audit.changed_paths == ('src/safe.py',)
    review_worktree(TaskWorkspace(root, workspace, brief), FakeAgent('reviewer', workspace))
    assert not request_worktree_approval(TaskWorkspace(root, workspace, brief)).is_symlink()
    assert phase(root, brief) is Phase.AWAITING_APPROVAL


def test_out_of_scope_write_blocks_review(setup: tuple[Path, Path, Path, Brief]) -> None:
    root, repo, workspace, brief = setup
    prepare_worktree(TaskWorkspace(root, workspace, brief), repo)
    with pytest.raises(WorktreeError, match='outside ownership'):
        delegate_in_worktree(
            TaskWorkspace(root, workspace, brief), FakeAgent('worker', workspace, filename='OTHER')
        )
    with pytest.raises(DelegationError, match='uncertain dispatch'):
        review_worktree(TaskWorkspace(root, workspace, brief), FakeAgent('reviewer', workspace))


def test_worker_change_after_audit_blocks_review(setup: tuple[Path, Path, Path, Brief]) -> None:
    root, repo, workspace, brief = setup
    prepare_worktree(TaskWorkspace(root, workspace, brief), repo)
    delegate_in_worktree(
        TaskWorkspace(root, workspace, brief),
        FakeAgent('worker', workspace, filename='src/safe.py'),
    )
    (workspace / 'other.txt').write_text('drift\n')
    with pytest.raises(WorktreeError):
        review_worktree(TaskWorkspace(root, workspace, brief), FakeAgent('reviewer', workspace))


def test_review_without_audit_is_rejected(setup: tuple[Path, Path, Path, Brief]) -> None:
    root, repo, workspace, brief = setup
    prepare_worktree(TaskWorkspace(root, workspace, brief), repo)
    with pytest.raises(DelegationError, match='uncertain dispatch'):
        review_worktree(TaskWorkspace(root, workspace, brief), FakeAgent('reviewer', workspace))


def test_cannot_reprepare_existing_worktree(setup: tuple[Path, Path, Path, Brief]) -> None:
    root, repo, workspace, brief = setup
    prepare_worktree(TaskWorkspace(root, workspace, brief), repo)
    with pytest.raises(WorktreeError):
        prepare_worktree(TaskWorkspace(root, workspace, brief), repo)


def test_audit_tampering_blocks_review(setup: tuple[Path, Path, Path, Brief]) -> None:
    root, repo, workspace, brief = setup
    prepare_worktree(TaskWorkspace(root, workspace, brief), repo)
    delegate_in_worktree(
        TaskWorkspace(root, workspace, brief),
        FakeAgent('worker', workspace, filename='src/safe.py'),
    )
    (root / brief.task_id / 'worktree-audit.json').write_text('{}')
    with pytest.raises(DelegationError, match='uncertain dispatch'):
        review_worktree(TaskWorkspace(root, workspace, brief), FakeAgent('reviewer', workspace))


def test_same_path_content_change_blocks_review(setup: tuple[Path, Path, Path, Brief]) -> None:
    root, repo, workspace, brief = setup
    prepare_worktree(TaskWorkspace(root, workspace, brief), repo)
    delegate_in_worktree(
        TaskWorkspace(root, workspace, brief),
        FakeAgent('worker', workspace, filename='src/safe.py'),
    )
    (workspace / 'src/safe.py').write_text('altered after audit\n')
    with pytest.raises(DelegationError, match='evidence changed'):
        review_worktree(TaskWorkspace(root, workspace, brief), FakeAgent('reviewer', workspace))


def test_coordinator_success_and_duplicate_refusal(
    setup: tuple[Path, Path, Path, Brief],
) -> None:
    root, repo, workspace, brief = setup
    task = TaskWorkspace(root, workspace, brief)
    worker = FakeAgent('worker', workspace, filename='src/safe.py')
    reviewer = FakeAgent('reviewer', workspace)
    agents = OfflineAgents(worker, reviewer)
    result = run_offline_task(task, repo, agents)
    assert result.state is OrchestrationState.AWAITING_APPROVAL
    assert result.approval_request is not None
    assert b'"approved":false' in result.approval_request.read_bytes()
    assert (worker.calls, reviewer.calls) == (1, 1)
    assert inspect_offline_task(task).state is OrchestrationState.AWAITING_APPROVAL
    with pytest.raises((DelegationError, WorktreeError)):
        run_offline_task(task, repo, agents)
    assert (worker.calls, reviewer.calls) == (1, 1)


def test_coordinator_worker_failure_is_not_retried(
    setup: tuple[Path, Path, Path, Brief],
) -> None:
    class FailingAgent(FakeAgent):
        def run(self, brief: Brief, prior: Finding | None) -> Finding:
            del brief, prior
            self.calls += 1
            msg = 'offline fixture failure'
            raise RuntimeError(msg)

    root, repo, workspace, brief = setup
    task = TaskWorkspace(root, workspace, brief)
    worker = FailingAgent('worker', workspace)
    reviewer = FakeAgent('reviewer', workspace)
    with pytest.raises(RuntimeError, match='offline fixture failure'):
        run_offline_task(task, repo, OfflineAgents(worker, reviewer))
    assert inspect_offline_task(task).state is OrchestrationState.DISPATCH_UNCERTAIN
    with pytest.raises((DelegationError, WorktreeError)):
        run_offline_task(task, repo, OfflineAgents(worker, reviewer))
    assert (worker.calls, reviewer.calls) == (1, 0)


def test_coordinator_rejects_same_agent_instance(
    setup: tuple[Path, Path, Path, Brief],
) -> None:
    root, repo, workspace, brief = setup
    agent = FakeAgent('worker', workspace)
    with pytest.raises(DelegationError, match='distinct identities'):
        run_offline_task(TaskWorkspace(root, workspace, brief), repo, OfflineAgents(agent, agent))
    assert not (root / brief.task_id).exists()


def test_coordinator_reviewer_failure_requires_manual_review(
    setup: tuple[Path, Path, Path, Brief],
) -> None:
    class FailingReviewer(FakeAgent):
        def run(self, brief: Brief, prior: Finding | None) -> Finding:
            del brief
            assert prior is not None
            self.calls += 1
            msg = 'review interrupted'
            raise RuntimeError(msg)

    root, repo, workspace, brief = setup
    task = TaskWorkspace(root, workspace, brief)
    worker = FakeAgent('worker', workspace, filename='src/safe.py')
    reviewer = FailingReviewer('reviewer', workspace)
    with pytest.raises(RuntimeError, match='review interrupted'):
        run_offline_task(task, repo, OfflineAgents(worker, reviewer))
    assert inspect_offline_task(task).state is OrchestrationState.REVIEW_UNCERTAIN
    assert not (root / brief.task_id / 'approval-request.json').exists()
    with pytest.raises((DelegationError, WorktreeError)):
        run_offline_task(task, repo, OfflineAgents(worker, reviewer))
    assert (worker.calls, reviewer.calls) == (1, 1)
