"""Fail-closed comparison between integration snapshots and test evidence.

This module only evaluates evidence written by a separately qualified verifier;
it does not launch tests or treat self-reported model success as proof.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from hermes_harness.multiagent.verification_manifest import VerificationManifest

SHA256_HEX_LENGTH = 64


@dataclass(frozen=True, slots=True)
class TestEvidence:
    __test__ = False
    baseline: str
    head: str
    changed_paths: tuple[str, ...]
    file_digests: tuple[tuple[str, str | None], ...]
    verifier_identity: str
    exit_code: int
    output_sha256: str


@dataclass(frozen=True, slots=True)
class VerificationDecision:
    accepted: bool
    reason: str
    authorizes_merge: bool = False


def evaluate_test_evidence(
    manifest: VerificationManifest, evidence: TestEvidence
) -> VerificationDecision:
    """Check exact snapshot identity before admitting a reported test result."""
    if not evidence.verifier_identity.strip():
        msg = 'Missing independent verifier identity'
        raise WorkflowError(msg)
    if (
        evidence.baseline != manifest.baseline  # noqa: PLR0916
        or evidence.head != manifest.head
        or evidence.changed_paths != manifest.changed_paths
        or evidence.file_digests != manifest.file_digests
    ):
        msg = 'Verifier evidence does not match integration snapshot'
        raise WorkflowError(msg)
    if len(evidence.output_sha256) != SHA256_HEX_LENGTH or any(
        c not in '0123456789abcdef' for c in evidence.output_sha256
    ):
        msg = 'Invalid verifier output digest'
        raise WorkflowError(msg)
    if evidence.exit_code != 0:
        return VerificationDecision(accepted=False, reason='Acceptance checks reported failure')
    return VerificationDecision(
        accepted=True, reason='Evidence matched; external verifier reported success'
    )
