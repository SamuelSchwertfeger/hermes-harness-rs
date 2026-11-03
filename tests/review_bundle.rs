#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Read-only review evidence tests, including tamper/failure outcomes.

use std::path::{Path, PathBuf};

use hermes_harness::multiagent::evidence::review_bundle;
use serde_json::json;

const SHA256_HEX_LENGTH: usize = 64;

fn run(tmp: &Path) -> PathBuf {
    let directory = tmp.join("run");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("plan.json"),
        json!({"nodes": [{"id": "a"}]}).to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.join("decision.json"),
        json!({"state": "pending"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.join("a.receipt.json"),
        json!({
            "state": "awaiting_approval",
            "task_id": "run-a",
            "worker_sha256": "worker",
            "review_sha256": "review",
        })
        .to_string(),
    )
    .unwrap();
    directory
}

#[test]
fn review_bundle_is_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let directory = run(tmp.path());
    let result = review_bundle(tmp.path(), "run").unwrap();
    assert!(!result.authorizes_merge);
    assert_eq!(result.receipt_evidence[0].node, "a");
    assert_eq!(
        result.receipt_evidence[0].receipt_sha256.len(),
        SHA256_HEX_LENGTH
    );
    assert!(!directory.join("human-decision.json").exists());
}

#[test]
fn rejects_bad_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let directory = run(tmp.path());
    std::fs::write(
        directory.join("a.receipt.json"),
        json!({"state": "failed"}).to_string(),
    )
    .unwrap();
    let error = review_bundle(tmp.path(), "run").unwrap_err();
    assert!(error.to_string().contains("fully completed"));
}

#[cfg(unix)]
#[test]
fn rejects_symlink_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let directory = run(tmp.path());
    let receipt = directory.join("a.receipt.json");
    std::fs::remove_file(&receipt).unwrap();
    std::os::unix::fs::symlink(directory.join("plan.json"), &receipt).unwrap();
    assert!(review_bundle(tmp.path(), "run").is_err());
}
