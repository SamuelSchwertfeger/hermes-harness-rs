"""Integration candidate preflight is read-only and fail-closed."""

import json
import os
import shutil
import subprocess
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.integration_manifest import (
    IntegrationCandidate,
    materialize_integration,
    prepare_integration,
)
from hermes_harness.multiagent.model_edits import parse_edit_proposal
from hermes_harness.multiagent.model_pipeline import ProposalReviewResult
from hermes_harness.multiagent.model_review import ModelReview
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path

BASELINE = 'a' * 40


def _candidate(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    node: str, path: str, baseline: str = BASELINE, verdict: str = 'accept'
) -> IntegrationCandidate:
    proposal = parse_edit_proposal(
        json.dumps({'task_id': node, 'files': [{'path': path, 'content': node}]}),
        node,
        ('src',),
    )
    review = ModelReview(node, 'reviewer', verdict, ())
    return IntegrationCandidate(node, baseline, ProposalReviewResult(proposal, review))


def test_combines_disjoint_reviewed_candidates() -> None:
    result = prepare_integration((
        _candidate('one', 'src/a.py'),
        _candidate('two', 'src/b.py'),
    ))
    assert result.nodes == ('one', 'two')
    assert result.files == ('src/a.py', 'src/b.py')
    assert result.integrated is False


def test_rejects_divergent_baseline() -> None:
    with pytest.raises(WorkflowError, match='Divergent'):
        prepare_integration((
            _candidate('one', 'src/a.py'),
            _candidate('two', 'src/b.py', 'b' * 40),
        ))


def test_rejects_conflicting_paths() -> None:
    with pytest.raises(WorkflowError, match='Overlapping'):
        prepare_integration((_candidate('one', 'src/a.py'), _candidate('two', 'src/a.py')))


def test_rejects_unapproved_review() -> None:
    with pytest.raises(WorkflowError, match='accepting'):
        prepare_integration((_candidate('one', 'src/a.py', verdict='changes_required'),))


def _git(repo: Path, *args: str) -> str:
    git = shutil.which('git')
    assert git is not None
    completed = subprocess.run(  # noqa: S603 - bounded temporary fixture Git
        [os.path.realpath(git), '-C', str(repo), *args],
        check=True,
        capture_output=True,
        shell=False,
    )
    return completed.stdout.decode().strip()


def test_materializes_multiple_disjoint_files_in_integration_worktree(tmp_path: Path) -> None:
    repo = tmp_path / 'repo'
    repo.mkdir()
    _git(repo, 'init', '-q', '-b', 'main')
    _git(
        repo,
        '-c',
        'user.name=Test',
        '-c',
        'user.email=test@example.invalid',
        'commit',
        '--allow-empty',
        '-qm',
        'baseline',
    )
    baseline = _git(repo, 'rev-parse', 'HEAD')
    worktree = tmp_path / 'integration'
    _git(repo, 'worktree', 'add', '-b', 'harness/integration', str(worktree), 'HEAD')
    candidates = (
        _candidate('one', 'src/a.py', baseline),
        _candidate('two', 'src/b.py', baseline),
    )
    with pytest.raises(WorkflowError, match='authorization'):
        materialize_integration(worktree, candidates)
    result = materialize_integration(worktree, candidates, authorized=True)
    assert result.audit.changed_paths == ('src/a.py', 'src/b.py')
    assert (worktree / 'src/a.py').read_text() == 'one'
    assert not (repo / 'src/a.py').exists()
