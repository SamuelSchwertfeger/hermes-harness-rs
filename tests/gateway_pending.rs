#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use hermes_harness::gateway::gateway_pending::{PendingDisposition, inspect_pending};
use hermes_harness::gateway::gateway_readonly::SessionSnapshot;
use serde_json::{Value, json};

fn snapshot(pending: Value, session_id: &str) -> SessionSnapshot {
    let mut data = serde_json::Map::new();
    data.insert("pending".to_owned(), pending);
    SessionSnapshot {
        session_id: session_id.to_owned(),
        data,
    }
}

#[test]
fn unique_pending_match() {
    let snap = snapshot(
        json!([
            {"input_id": "other", "admission_id": "adm-2"},
            {"input_id": "sub-1", "admission_id": "adm-1"},
        ]),
        "session-1",
    );
    let result = inspect_pending(&snap, "session-1", "sub-1");
    assert_eq!(result.disposition, PendingDisposition::Found);
    assert_eq!(result.admission_id.as_deref(), Some("adm-1"));
}

#[test]
fn missing_pending_is_unknown() {
    let result = inspect_pending(&snapshot(json!([]), "session-1"), "session-1", "sub-1");
    assert_eq!(result.disposition, PendingDisposition::Indeterminate);
}

#[test]
fn duplicate_match_is_unknown() {
    let snap = snapshot(
        json!([
            {"input_id": "sub-1", "admission_id": "a"},
            {"input_id": "sub-1", "admission_id": "b"},
        ]),
        "session-1",
    );
    let result = inspect_pending(&snap, "session-1", "sub-1");
    assert_eq!(result.disposition, PendingDisposition::Indeterminate);
}

#[test]
fn invalid_identity_is_unknown() {
    let snap = snapshot(
        json!([{"input_id": "sub-1", "admission_id": ""}]),
        "session-1",
    );
    assert_eq!(
        inspect_pending(&snap, "session-1", "sub-1").disposition,
        PendingDisposition::Indeterminate
    );
}

#[test]
fn wrong_session_is_unknown() {
    let snap = snapshot(
        json!([{"input_id": "sub-1", "admission_id": "a"}]),
        "session-1",
    );
    assert_eq!(
        inspect_pending(&snap, "session-2", "sub-1").disposition,
        PendingDisposition::Indeterminate
    );
}

#[test]
fn malformed_pending_is_unknown() {
    assert_eq!(
        inspect_pending(
            &snapshot(json!("not-a-list"), "session-1"),
            "session-1",
            "sub-1"
        )
        .disposition,
        PendingDisposition::Indeterminate
    );
    let result = inspect_pending(&snapshot(json!([42]), "session-1"), "session-1", "sub-1");
    assert_eq!(result.disposition, PendingDisposition::Indeterminate);
}
