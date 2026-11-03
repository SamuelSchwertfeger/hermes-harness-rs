"""Local reviewed-replacement pipeline authorization and review tests."""

import hashlib
import json
import os
import shutil
import subprocess
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.local_inference import LocalEndpoint, LocalInferenceError
from hermes_harness.multiagent.replacement_pipeline import (
    apply_reviewed_replacements,
    generate_reviewed_replacements,
)
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.multiagent.replacement_pipeline import ReplacementReview


class FakeTransport:
    def __init__(self, digest: str, verdict: str = 'accept') -> None:
        self.digest = digest
        self.verdict = verdict
        self.calls: list[str] = []

    def complete(self, endpoint: LocalEndpoint, _payload: bytes) -> bytes:
        self.calls.append(endpoint.model)
        if endpoint.model == 'worker':
            text = json.dumps({
                'task_id': 't',
                'replacements': [
                    {
                        'path': 'src/a.py',
                        'expected_sha256': self.digest,
                        'content': 'new = 2\n',
                    }
                ],
            })
        else:
            text = json.dumps({'verdict': self.verdict, 'findings': ['checked']})
        return json.dumps({'choices': [{'message': {'content': text}}]}).encode()


def _endpoint(name: str) -> LocalEndpoint:
    return LocalEndpoint('http://127.0.0.1:11434/v1/chat/completions', name)


def _run(verdict: str = 'accept') -> tuple[ReplacementReview, FakeTransport]:
    digest = hashlib.sha256(b'old = 1\n').hexdigest()
    transport = FakeTransport(digest, verdict)
    result = generate_reviewed_replacements(
        _endpoint('worker'),
        _endpoint('reviewer'),
        't',
        'Replace',
        ('src',),
        {'src/a.py': digest},
        transport,
        authorized=True,
    )
    return result, transport


def test_serial_review_admission() -> None:
    result, transport = _run()
    assert transport.calls == ['worker', 'reviewer']
    assert result.verdict == 'accept'
    assert result.applied is False


def test_rejected_review_blocks_application(tmp_path: Path) -> None:
    result, _ = _run('changes_required')
    with pytest.raises(WorkflowError, match='rejected'):
        apply_reviewed_replacements(
            result,
            tmp_path,
            ('src',),
            'unused',
            authorized=True,
        )


def test_separate_application_authorization(tmp_path: Path) -> None:
    result, _ = _run()
    with pytest.raises(WorkflowError, match='separate authorization'):
        apply_reviewed_replacements(result, tmp_path, ('src',), 'unused')


def test_same_model_rejected_before_inference() -> None:
    digest = hashlib.sha256(b'old').hexdigest()
    transport = FakeTransport(digest)
    with pytest.raises(LocalInferenceError, match='distinct'):
        generate_reviewed_replacements(
            _endpoint('worker'),
            _endpoint('worker'),
            't',
            'Replace',
            ('src',),
            {'src/a.py': digest},
            transport,
            authorized=True,
        )
    assert transport.calls == []


def _git(repo: Path, *args: str) -> str:
    git = shutil.which('git')
    assert git is not None
    result = subprocess.run(  # noqa: S603 - temporary Git fixture
        [os.path.realpath(git), '-C', str(repo), *args],
        capture_output=True,
        check=True,
        shell=False,
    )
    return result.stdout.decode().strip()


def test_reviewed_replacement_applies_only_in_disposable_worktree(tmp_path: Path) -> None:
    repo = tmp_path / 'repo'
    repo.mkdir()
    _git(repo, 'init', '-q', '-b', 'main')
    (repo / 'src').mkdir()
    (repo / 'src/a.py').write_text('old = 1\n')
    _git(repo, 'add', 'src/a.py')
    _git(
        repo,
        '-c',
        'user.name=Test',
        '-c',
        'user.email=test@example.invalid',
        'commit',
        '-qm',
        'baseline',
    )
    workspace = tmp_path / 'worktree'
    _git(repo, 'worktree', 'add', '-b', 'harness/replacement-test', str(workspace), 'HEAD')
    baseline = _git(workspace, 'rev-parse', 'HEAD')
    result, _ = _run()
    applied = apply_reviewed_replacements(
        result,
        workspace,
        ('src',),
        baseline,
        authorized=True,
    )
    assert applied.applied is True
    assert (workspace / 'src/a.py').read_text() == 'new = 2\n'
    assert (repo / 'src/a.py').read_text() == 'old = 1\n'


def test_replacement_reviewer_receives_original_objective() -> None:
    class CaptureTransport(FakeTransport):
        def complete(self, endpoint: LocalEndpoint, payload: bytes) -> bytes:
            if endpoint.model == 'reviewer':
                messages = json.loads(payload)['messages']
                data = json.loads(messages[1]['content'])
                assert data['objective'] == 'Fix only the incorrect addition function'
                assert 'unrelated' in messages[0]['content']
            return super().complete(endpoint, payload)

    digest = hashlib.sha256(b'old = 1\n').hexdigest()
    transport = CaptureTransport(digest)
    result = generate_reviewed_replacements(
        _endpoint('worker'),
        _endpoint('reviewer'),
        't',
        'Fix only the incorrect addition function',
        ('src',),
        {'src/a.py': digest},
        transport,
        authorized=True,
    )
    assert result.verdict == 'accept'
    assert transport.calls == ['worker', 'reviewer']
