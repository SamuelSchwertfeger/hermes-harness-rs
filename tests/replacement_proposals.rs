#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Schema/ownership validation for local model replacement proposals.

use hermes_harness::multiagent::replacement_proposals::parse_replacement_proposal;
use hermes_harness::util::sha256_hex;
use serde_json::json;

fn scopes() -> Vec<String> {
    vec!["src".to_owned()]
}

fn payload(path: &str) -> String {
    json!({
        "task_id": "task-a",
        "replacements": [{
            "path": path,
            "expected_sha256": sha256_hex(b"old"),
            "content": "new",
        }],
    })
    .to_string()
}

#[test]
fn parse_scoped_replacement() {
    let result = parse_replacement_proposal(&payload("src/a.py"), "task-a", &scopes()).unwrap();
    assert_eq!(result[0].path, "src/a.py");
    assert_eq!(result[0].content, "new");
}

#[test]
fn rejects_unsafe_path() {
    for path in ["../escape", "/tmp/x", ".git/config", "other.py", "src/../x"] {
        assert!(
            parse_replacement_proposal(&payload(path), "task-a", &scopes()).is_err(),
            "{path}"
        );
    }
}

#[test]
fn rejects_mismatched_task() {
    let error = parse_replacement_proposal(&payload("src/a.py"), "task-b", &scopes()).unwrap_err();
    assert!(error.to_string().contains("task mismatch"));
}

#[test]
fn rejects_duplicate_json_fields() {
    let error =
        parse_replacement_proposal(r#"{"task_id":"x","task_id":"x"}"#, "x", &scopes()).unwrap_err();
    assert!(error.to_string().contains("Duplicate"));
}
