#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Tests for a durable integrity-only lifecycle checkpoint.

use std::path::{Path, PathBuf};

use hermes_harness::native::lifecycle::record_checkpoint;
use hermes_harness::util::sha256_hex;
use serde_json::{Value, json};

fn inputs(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let review = tmp.join("review.json");
    let verification = tmp.join("verified.json");
    let evidence = tmp.join("checkpoint.json");
    let content_hash = sha256_hex(b"example");
    std::fs::write(
        &review,
        json!({
            "mode": "live-reviewed-proposal-only",
            "task_id": "task-1",
            "head": "a".repeat(40),
            "repository": "/example/repo",
            "verdict": "accept",
            "applied": false,
            "human_decision_pending": true,
            "files": [{"path": "module.py", "sha256": content_hash}],
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        &verification,
        json!({
            "mode": "read-only-integrity-verification",
            "task_id": "task-1",
            "baseline": "a".repeat(40),
            "repository": "/example/repo",
            "workspace": "/example/worktree",
            "branch": "harness/task-1",
            "integrity_verified": true,
            "tests_executed": false,
            "generated_code_executed": false,
            "human_decision_pending": true,
            "changed_paths": ["module.py"],
            "files": [{"path": "module.py", "sha256": content_hash}],
        })
        .to_string(),
    )
    .unwrap();
    (review, verification, evidence)
}

fn edit(path: &Path, change: impl FnOnce(&mut Value)) {
    let mut data: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    change(&mut data);
    std::fs::write(path, data.to_string()).unwrap();
}

#[test]
fn checkpoint_records_linked_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let (review, verified, evidence) = inputs(tmp.path());
    let result = record_checkpoint(&review, &verified, &evidence, true).unwrap();
    assert_eq!(result["integrity_verified"], true);
    assert_eq!(result["functional_verified"], false);
    assert_eq!(
        result["next_stage"],
        "requires-separate-functional-execution-authorization"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = evidence.metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&evidence).unwrap()).unwrap();
    assert_eq!(
        written["review_receipt_sha256"],
        sha256_hex(&std::fs::read(&review).unwrap())
    );
}

#[test]
fn checkpoint_requires_authorization() {
    let tmp = tempfile::tempdir().unwrap();
    let (review, verified, evidence) = inputs(tmp.path());
    let error = record_checkpoint(&review, &verified, &evidence, false).unwrap_err();
    assert!(error.to_string().contains("authorization"), "{error}");
    assert!(!evidence.exists());
}

#[test]
fn checkpoint_rejects_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let (review, verified, evidence) = inputs(tmp.path());
    record_checkpoint(&review, &verified, &evidence, true).unwrap();
    let error = record_checkpoint(&review, &verified, &evidence, true).unwrap_err();
    assert!(error.to_string().contains("destination"), "{error}");
}

#[test]
fn checkpoint_rejects_mismatched_digest() {
    let tmp = tempfile::tempdir().unwrap();
    let (review, verified, evidence) = inputs(tmp.path());
    edit(&verified, |data| {
        data["files"][0]["sha256"] = json!("0".repeat(64));
    });
    let error = record_checkpoint(&review, &verified, &evidence, true).unwrap_err();
    assert!(error.to_string().contains("digests"), "{error}");
    assert!(!evidence.exists());
}

#[test]
fn checkpoint_rejects_false_functional_assertion() {
    let tmp = tempfile::tempdir().unwrap();
    let (review, verified, evidence) = inputs(tmp.path());
    edit(&verified, |data| data["tests_executed"] = json!(true));
    let error = record_checkpoint(&review, &verified, &evidence, true).unwrap_err();
    assert!(error.to_string().contains("integrity-only"), "{error}");
}

#[test]
fn checkpoint_rejects_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let (review, verified, evidence) = inputs(tmp.path());
    edit(&review, |data| {
        data["files"][0]["path"] = json!("../module.py")
    });
    let error = record_checkpoint(&review, &verified, &evidence, true).unwrap_err();
    assert!(error.to_string().contains("owned path"), "{error}");
}
