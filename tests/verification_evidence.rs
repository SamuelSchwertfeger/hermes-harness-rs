#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Acceptance evidence cannot be transplanted onto another integration snapshot.

use hermes_harness::multiagent::verification_evidence::{TestEvidence, evaluate_test_evidence};
use hermes_harness::multiagent::verification_manifest::VerificationManifest;

fn manifest() -> VerificationManifest {
    VerificationManifest {
        baseline: "a".repeat(40),
        head: "a".repeat(40),
        changed_paths: vec!["src/a.py".to_owned()],
        file_digests: vec![("src/a.py".to_owned(), Some("b".repeat(64)))],
        verified_by_tests: false,
    }
}

fn evidence(code: i32) -> TestEvidence {
    let manifest = manifest();
    TestEvidence {
        baseline: manifest.baseline,
        head: manifest.head,
        changed_paths: manifest.changed_paths,
        file_digests: manifest.file_digests,
        verifier_identity: "independent-verifier".to_owned(),
        exit_code: code,
        output_sha256: "c".repeat(64),
    }
}

#[test]
fn matching_success_never_authorizes_merge() {
    let result = evaluate_test_evidence(&manifest(), &evidence(0)).unwrap();
    assert!(result.accepted);
    assert!(!result.authorizes_merge);
}

#[test]
fn failed_test_report_is_rejected() {
    assert!(
        !evaluate_test_evidence(&manifest(), &evidence(1))
            .unwrap()
            .accepted
    );
}

#[test]
fn stale_snapshot_is_rejected() {
    let stale = TestEvidence {
        baseline: "d".repeat(40),
        head: "a".repeat(40),
        changed_paths: vec!["src/a.py".to_owned()],
        file_digests: vec![("src/a.py".to_owned(), Some("b".repeat(64)))],
        verifier_identity: "verifier".to_owned(),
        exit_code: 0,
        output_sha256: "c".repeat(64),
    };
    let error = evaluate_test_evidence(&manifest(), &stale).unwrap_err();
    assert!(error.to_string().contains("does not match"));
}
