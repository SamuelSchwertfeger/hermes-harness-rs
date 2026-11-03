#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Verification manifest does not execute or approve changed code.

use hermes_harness::multiagent::verification_manifest::prepare_verification;

#[test]
fn rejects_missing_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let error = prepare_verification(
        &dir.path().join("absent"),
        &"a".repeat(40),
        &["src".to_owned()],
    )
    .unwrap_err();
    assert!(error.to_string().contains("real worktree"));
}
