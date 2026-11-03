"""Read-only review evidence tests, including tamper/failure outcomes."""

import json
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.evidence import review_bundle
from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path

SHA256_HEX_LENGTH = 64


def _run(tmp_path: Path) -> Path:
    directory = tmp_path / 'run'
    directory.mkdir()
    (directory / 'plan.json').write_text(json.dumps({'nodes': [{'id': 'a'}]}), encoding='utf-8')
    (directory / 'decision.json').write_text(json.dumps({'state': 'pending'}), encoding='utf-8')
    (directory / 'a.receipt.json').write_text(
        json.dumps({
            'state': 'awaiting_approval',
            'task_id': 'run-a',
            'worker_sha256': 'worker',
            'review_sha256': 'review',
        }),
        encoding='utf-8',
    )
    return directory


def test_review_bundle_is_read_only(tmp_path: Path) -> None:
    directory = _run(tmp_path)
    result = review_bundle(tmp_path, 'run')
    assert result['authorizes_merge'] is False
    assert result['receipt_evidence'][0]['node'] == 'a'
    assert len(result['receipt_evidence'][0]['receipt_sha256']) == SHA256_HEX_LENGTH
    assert not (directory / 'human-decision.json').exists()


def test_rejects_bad_receipt(tmp_path: Path) -> None:
    directory = _run(tmp_path)
    receipt = directory / 'a.receipt.json'
    receipt.write_text(json.dumps({'state': 'failed'}), encoding='utf-8')
    with pytest.raises(WorkflowError, match='fully completed'):
        review_bundle(tmp_path, 'run')


def test_rejects_symlink_receipt(tmp_path: Path) -> None:
    directory = _run(tmp_path)
    receipt = directory / 'a.receipt.json'
    receipt.unlink()
    receipt.symlink_to(directory / 'plan.json')
    with pytest.raises(WorkflowError):
        review_bundle(tmp_path, 'run')
