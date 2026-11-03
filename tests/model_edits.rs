#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Model file proposal validation and ownership tests.

use hermes_harness::multiagent::model_edits::parse_edit_proposal;
use serde_json::json;

const SHA256_HEX_LENGTH: usize = 64;

fn scopes() -> Vec<String> {
    vec!["tests".to_owned()]
}

fn payload(path: &str) -> String {
    json!({
        "task_id": "ticket-a",
        "files": [{"path": path, "content": "def test_ok(): pass\n"}],
    })
    .to_string()
}

#[test]
fn accepted_proposal_is_data_only() {
    let proposal = parse_edit_proposal(&payload("tests/test_a.py"), "ticket-a", &scopes()).unwrap();
    assert_eq!(proposal.files[0].path, "tests/test_a.py");
    assert_eq!(proposal.files[0].sha256.len(), SHA256_HEX_LENGTH);
}

#[test]
fn rejects_unowned_or_unsafe_paths() {
    for path in [
        "../escape",
        "/etc/passwd",
        ".git/config",
        "src/unowned.py",
        "tests/../other.py",
        "tests//bad.py",
    ] {
        let error = parse_edit_proposal(&payload(path), "ticket-a", &scopes()).unwrap_err();
        assert!(
            error.to_string().contains("forbidden or unowned"),
            "{path}: {error}"
        );
    }
}

#[test]
fn identity_and_duplicate_paths_fail() {
    let error = parse_edit_proposal(&payload("tests/a.py"), "other", &scopes()).unwrap_err();
    assert!(error.to_string().contains("identity mismatch"));
    let duplicate = json!({
        "task_id": "ticket-a",
        "files": [
            {"path": "tests/a.py", "content": "a"},
            {"path": "tests/a.py", "content": "b"},
        ],
    })
    .to_string();
    let error = parse_edit_proposal(&duplicate, "ticket-a", &scopes()).unwrap_err();
    assert!(error.to_string().contains("Duplicate"));
}
