"""Fail-closed outcome and dependency state tests for the multiagent runner."""

import json
from typing import TYPE_CHECKING

from hermes_harness.multiagent.runtime import _task_status

if TYPE_CHECKING:
    from pathlib import Path


def test_receipt_only_counts_as_completed_on_approval_ready(tmp_path: Path) -> None:
    receipt = tmp_path / 'step.receipt.json'
    receipt.write_text(json.dumps({'state': 'awaiting_approval'}), encoding='utf-8')
    assert _task_status(tmp_path, 'step')['state'] == 'completed'
    receipt.write_text(json.dumps({'state': 'manual_intervention'}), encoding='utf-8')
    assert _task_status(tmp_path, 'step')['state'] == 'failed'
    receipt.write_text(json.dumps({'state': 'dispatch_uncertain'}), encoding='utf-8')
    assert _task_status(tmp_path, 'step')['state'] == 'failed'


def test_fence_without_receipt_is_unknown(tmp_path: Path) -> None:
    assert _task_status(tmp_path, 'step')['state'] == 'pending'
    (tmp_path / 'step.dispatch-fence.json').write_text('{}', encoding='utf-8')
    assert _task_status(tmp_path, 'step')['state'] == 'unknown'
