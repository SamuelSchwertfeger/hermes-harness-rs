"""Receipt admission tests: no model calls or worktree writes."""

import hashlib
import json
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.worktree_apply import _admitted_proposal, apply_reviewed_receipt

if TYPE_CHECKING:
    from pathlib import Path


CONTENT = 'def add(a: int, b: int) -> int:\n    return a + b\n'
DIFF = (
    '--- /dev/null\n+++ b/smoke_math.py\n@@ -0,0 +1,2 @@\n'
    '+def add(a: int, b: int) -> int:\n+    return a + b\n'
)


def _record() -> dict[str, object]:
    return {
        'task_id': 'new-smoke',
        'files': [
            {
                'path': 'smoke_math.py',
                'sha256': hashlib.sha256(CONTENT.encode()).hexdigest(),
                'diff': DIFF,
            }
        ],
    }


def test_receipt_admits_matching_hash_and_content() -> None:
    task, owned, proposal = _admitted_proposal(_record())
    assert task == 'new-smoke'
    assert owned == ('smoke_math.py',)
    assert proposal.files[0].content == CONTENT


def test_receipt_rejects_tampered_diff() -> None:
    record = _record()
    rows = record['files']
    assert isinstance(rows, list)
    rows[0]['diff'] = DIFF.replace('a + b', 'a - b')
    with pytest.raises(WorkflowError, match='hash mismatch'):
        _admitted_proposal(record)


def test_receipt_rejects_path_escape() -> None:
    record = _record()
    rows = record['files']
    assert isinstance(rows, list)
    rows[0]['path'] = '../escape.py'
    rows[0]['diff'] = DIFF.replace('smoke_math.py', '../escape.py')
    with pytest.raises(WorkflowError, match=r'unowned|forbidden'):
        _admitted_proposal(record)


def test_rejected_review_never_creates_worktree(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from hermes_harness.native import worktree_apply  # noqa: PLC0415

    monkeypatch.setattr(worktree_apply, 'repository', lambda path: path)
    receipt = tmp_path / 'receipt.json'
    receipt.write_text(
        json.dumps({
            **_record(),
            'repository': str(tmp_path),
            'mode': 'live-reviewed-proposal-only',
            'verdict': 'changes_required',
            'applied': False,
            'human_decision_pending': True,
        })
    )
    workspace = tmp_path / 'workspace'
    with pytest.raises(WorkflowError, match='not eligible'):
        apply_reviewed_receipt(repo=tmp_path, receipt=receipt, workspace=workspace, authorized=True)
    assert not workspace.exists()


def test_application_without_grant_does_not_touch_filesystem(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='authorization'):
        apply_reviewed_receipt(
            repo=tmp_path,
            receipt=tmp_path / 'missing.json',
            workspace=tmp_path / 'workspace',
            authorized=False,
        )
    assert not (tmp_path / 'workspace').exists()
