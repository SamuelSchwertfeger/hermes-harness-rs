"""Verification manifest does not execute or approve changed code."""

from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.multiagent.verification_manifest import prepare_verification

if TYPE_CHECKING:
    from pathlib import Path


def test_rejects_missing_worktree(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='real worktree'):
        prepare_verification(tmp_path / 'absent', 'a' * 40, ('src',))
