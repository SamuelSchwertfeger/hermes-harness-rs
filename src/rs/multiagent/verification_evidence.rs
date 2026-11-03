//! Fail-closed comparison between integration snapshots and test evidence.
//!
//! This module only evaluates evidence written by a separately qualified verifier;
//! it does not launch tests or treat self-reported model success as proof.

use crate::multiagent::runtime::WorkflowError;
use crate::multiagent::verification_manifest::VerificationManifest;
use crate::util::is_py_blank;

/// Length of a SHA-256 hex digest.
pub const SHA256_HEX_LENGTH: usize = 64;

/// Result reported by an independent verifier for one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TestEvidence {
    /// Baseline commit the verifier saw.
    pub baseline: String,
    /// `HEAD` commit the verifier saw.
    pub head: String,
    /// Changed paths the verifier saw.
    pub changed_paths: Vec<String>,
    /// File digests the verifier saw.
    pub file_digests: Vec<(String, Option<String>)>,
    /// Identity of the independent verifier.
    pub verifier_identity: String,
    /// Test process exit code.
    pub exit_code: i32,
    /// SHA-256 over `stdout + NUL + stderr`.
    pub output_sha256: String,
}

/// Outcome of comparing evidence to a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VerificationDecision {
    /// Whether the evidence matched and reported success.
    pub accepted: bool,
    /// Human-readable reason.
    pub reason: String,
    /// Never true: merging needs a separate decision.
    pub authorizes_merge: bool,
}

/// Check exact snapshot identity before admitting a reported test result.
///
/// # Errors
/// Returns [`WorkflowError`] for a missing verifier identity, a snapshot mismatch or an
/// invalid output digest.
pub fn evaluate_test_evidence(
    manifest: &VerificationManifest,
    evidence: &TestEvidence,
) -> Result<VerificationDecision, WorkflowError> {
    if is_py_blank(&evidence.verifier_identity) {
        return Err(WorkflowError::msg("Missing independent verifier identity"));
    }
    if evidence.baseline != manifest.baseline
        || evidence.head != manifest.head
        || evidence.changed_paths != manifest.changed_paths
        || evidence.file_digests != manifest.file_digests
    {
        return Err(WorkflowError::msg(
            "Verifier evidence does not match integration snapshot",
        ));
    }
    let digest = &evidence.output_sha256;
    if digest.chars().count() != SHA256_HEX_LENGTH
        || digest.chars().any(|c| !matches!(c, '0'..='9' | 'a'..='f'))
    {
        return Err(WorkflowError::msg("Invalid verifier output digest"));
    }
    if evidence.exit_code != 0 {
        return Ok(VerificationDecision {
            accepted: false,
            reason: "Acceptance checks reported failure".to_owned(),
            authorizes_merge: false,
        });
    }
    Ok(VerificationDecision {
        accepted: true,
        reason: "Evidence matched; external verifier reported success".to_owned(),
        authorizes_merge: false,
    })
}
