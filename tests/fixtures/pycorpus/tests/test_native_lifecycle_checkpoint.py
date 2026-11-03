"""Tests for a durable integrity-only lifecycle checkpoint."""

import hashlib
import json
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.lifecycle import record_checkpoint

if TYPE_CHECKING:
    from pathlib import Path


def _inputs(tmp_path: Path) -> tuple[Path, Path, Path]:
    root = tmp_path / 'review.json'
    verification = tmp_path / 'verified.json'
    evidence = tmp_path / 'checkpoint.json'
    content_hash = hashlib.sha256(b'example').hexdigest()
    root.write_text(
        json.dumps({
            'mode': 'live-reviewed-proposal-only',
            'task_id': 'task-1',
            'head': 'a' * 40,
            'repository': '/example/repo',
            'verdict': 'accept',
            'applied': False,
            'human_decision_pending': True,
            'files': [{'path': 'module.py', 'sha256': content_hash}],
        })
    )
    verification.write_text(
        json.dumps({
            'mode': 'read-only-integrity-verification',
            'task_id': 'task-1',
            'baseline': 'a' * 40,
            'repository': '/example/repo',
            'workspace': '/example/worktree',
            'branch': 'harness/task-1',
            'integrity_verified': True,
            'tests_executed': False,
            'generated_code_executed': False,
            'human_decision_pending': True,
            'changed_paths': ['module.py'],
            'files': [{'path': 'module.py', 'sha256': content_hash}],
        })
    )
    return root, verification, evidence


def test_checkpoint_records_linked_evidence(tmp_path: Path) -> None:
    review, verified, evidence = _inputs(tmp_path)
    result = record_checkpoint(
        receipt=review, verification=verified, evidence=evidence, authorized=True
    )
    assert result['integrity_verified'] is True
    assert result['functional_verified'] is False
    assert result['next_stage'] == 'requires-separate-functional-execution-authorization'
    assert evidence.stat().st_mode & 0o777 == 0o600  # noqa: PLR2004 - evidence permissions
    assert (
        json.loads(evidence.read_text())['review_receipt_sha256']
        == hashlib.sha256(review.read_bytes()).hexdigest()
    )


def test_checkpoint_requires_authorization(tmp_path: Path) -> None:
    review, verified, evidence = _inputs(tmp_path)
    with pytest.raises(WorkflowError, match='authorization'):
        record_checkpoint(receipt=review, verification=verified, evidence=evidence)
    assert not evidence.exists()


def test_checkpoint_rejects_replay(tmp_path: Path) -> None:
    review, verified, evidence = _inputs(tmp_path)
    record_checkpoint(receipt=review, verification=verified, evidence=evidence, authorized=True)
    with pytest.raises(WorkflowError, match='destination'):
        record_checkpoint(receipt=review, verification=verified, evidence=evidence, authorized=True)


def test_checkpoint_rejects_mismatched_digest(tmp_path: Path) -> None:
    review, verified, evidence = _inputs(tmp_path)
    data = json.loads(verified.read_text())
    data['files'][0]['sha256'] = '0' * 64
    verified.write_text(json.dumps(data))
    with pytest.raises(WorkflowError, match='digests'):
        record_checkpoint(receipt=review, verification=verified, evidence=evidence, authorized=True)
    assert not evidence.exists()


def test_checkpoint_rejects_false_functional_assertion(tmp_path: Path) -> None:
    review, verified, evidence = _inputs(tmp_path)
    data = json.loads(verified.read_text())
    data['tests_executed'] = True
    verified.write_text(json.dumps(data))
    with pytest.raises(WorkflowError, match='integrity-only'):
        record_checkpoint(receipt=review, verification=verified, evidence=evidence, authorized=True)


def test_checkpoint_rejects_traversal(tmp_path: Path) -> None:
    review, verified, evidence = _inputs(tmp_path)
    data = json.loads(review.read_text())
    data['files'][0]['path'] = '../module.py'
    review.write_text(json.dumps(data))
    with pytest.raises(WorkflowError, match='owned path'):
        record_checkpoint(receipt=review, verification=verified, evidence=evidence, authorized=True)
