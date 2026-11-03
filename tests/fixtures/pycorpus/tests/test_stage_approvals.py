"""Approval checkpoints enforce ordered, immutable stage evidence."""

import subprocess
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.stage_approvals import approve, begin, snapshot, submit

if TYPE_CHECKING:
    from pathlib import Path


def test_four_stage_checkpoint_flow(tmp_path: Path) -> None:
    repo = tmp_path / 'repo'
    repo.mkdir()
    subprocess.run(['/usr/bin/git', 'init', '-qb', 'main', str(repo)], check=True)  # noqa: S603
    (repo / 'README.md').write_text('hello')
    subprocess.run(['/usr/bin/git', '-C', str(repo), 'add', 'README.md'], check=True)  # noqa: S603
    subprocess.run(  # noqa: S603
        [
            '/usr/bin/git',
            '-C',
            str(repo),
            '-c',
            'user.name=Test',
            '-c',
            'user.email=test@example.invalid',
            'commit',
            '-qm',
            'init',
        ],
        check=True,
    )
    state = tmp_path / 'state'
    state.mkdir()
    evidence = tmp_path / 'evidence.json'
    evidence.write_text('{"verified":true}')
    assert begin(state, 'ticket-1', repo, 'hermes-session')['next_stage'] == 'lets-investigate'
    with pytest.raises(WorkflowError, match='out of order'):
        submit(state, 'ticket-1', 'agents-assemble', evidence, 'hermes-session')
    for stage in ('lets-investigate', 'what-we-know', 'agents-assemble', 'stick-the-landing'):
        pending = submit(state, 'ticket-1', stage, evidence, 'hermes-session')
        assert pending['approval_pending']
        digest = pending['evidence']['sha256']
        with pytest.raises(WorkflowError, match='operator'):
            approve(state, 'ticket-1', stage, digest, 'hermes-session')
        with pytest.raises(WorkflowError, match='digest'):
            approve(state, 'ticket-1', stage, 'wrong', 'hermes-session', operator_confirmed=True)
        approve(state, 'ticket-1', stage, digest, 'hermes-session', operator_confirmed=True)
    assert snapshot(state, 'ticket-1')['finished'] is True
    with pytest.raises(WorkflowError, match='out of order'):
        submit(state, 'ticket-1', 'lets-investigate', evidence, 'hermes-session')
