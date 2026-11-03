#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Independent reviewer verdict rejects untrusted or unbound output.

use hermes_harness::supervised::live_reviewer_verdict::{
    Decision, ReviewerVerdict, ReviewerVerdictError, VerdictBinding, parse_reviewer_verdict,
};
use serde_json::{Value, json};

fn payload(changes: &[(&str, Value)]) -> String {
    let mut data = json!({
        "task_id": "task",
        "worker_submission_id": "worker-1",
        "audit_sha256": "a".repeat(64),
        "decision": "accept",
        "findings": [],
    });
    for (key, value) in changes {
        data[*key] = value.clone();
    }
    data.to_string()
}

fn parse(text: &str) -> Result<ReviewerVerdict, ReviewerVerdictError> {
    parse_reviewer_verdict(
        text,
        &VerdictBinding {
            task_id: "task".to_owned(),
            worker_submission_id: "worker-1".to_owned(),
            audit_sha256: "a".repeat(64),
        },
    )
}

#[test]
fn valid_acceptance_is_not_human_approval() {
    let verdict = parse(&payload(&[])).unwrap();
    assert_eq!(verdict.decision, Decision::Accept);
    assert_eq!(verdict.decision.as_str(), "accept");
}

#[test]
fn changes_requested_requires_findings() {
    let err = parse(&payload(&[("decision", json!("changes_requested"))])).unwrap_err();
    assert!(err.to_string().contains("requires findings"));
}

#[test]
fn identity_mismatch_rejected() {
    let changes = [
        ("task_id", json!("other")),
        ("worker_submission_id", json!("other")),
        ("audit_sha256", json!("b".repeat(64))),
    ];
    for change in changes {
        let err = parse(&payload(&[change])).unwrap_err();
        assert!(err.to_string().contains("not bound"));
    }
}

#[test]
fn unknown_field_rejected() {
    let err = parse(&payload(&[("approved", json!(true))])).unwrap_err();
    assert!(err.to_string().contains("schema"));
}

#[test]
fn non_json_and_oversize_rejected() {
    let err = parse("not json").unwrap_err();
    assert!(err.to_string().contains("not valid JSON"));
    let err = parse(&"x".repeat(20000)).unwrap_err();
    assert!(err.to_string().contains("bounded size"));
}

#[test]
fn invalid_findings_rejected() {
    let err = parse(&payload(&[("findings", json!([""]))])).unwrap_err();
    assert!(err.to_string().contains("findings"));
}
