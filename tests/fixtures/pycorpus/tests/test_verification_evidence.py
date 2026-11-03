"""Acceptance evidence cannot be transplanted onto another integration snapshot."""

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.multiagent.verification_evidence import (
    TestEvidence,
    evaluate_test_evidence,
)
from hermes_harness.multiagent.verification_manifest import VerificationManifest


def _manifest() -> VerificationManifest:
    return VerificationManifest('a' * 40, 'a' * 40, ('src/a.py',), (('src/a.py', 'b' * 64),))


def _evidence(code: int = 0) -> TestEvidence:
    manifest = _manifest()
    return TestEvidence(
        manifest.baseline,
        manifest.head,
        manifest.changed_paths,
        manifest.file_digests,
        'independent-verifier',
        code,
        'c' * 64,
    )


def test_matching_success_never_authorizes_merge() -> None:
    result = evaluate_test_evidence(_manifest(), _evidence())
    assert result.accepted
    assert result.authorizes_merge is False


def test_failed_test_report_is_rejected() -> None:
    assert not evaluate_test_evidence(_manifest(), _evidence(1)).accepted


def test_stale_snapshot_is_rejected() -> None:
    stale = TestEvidence(
        'd' * 40, 'a' * 40, ('src/a.py',), (('src/a.py', 'b' * 64),), 'verifier', 0, 'c' * 64
    )
    with pytest.raises(WorkflowError, match='does not match'):
        evaluate_test_evidence(_manifest(), stale)
