#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Plan admission regression tests for structured coordinator output.

use hermes_harness::multiagent::plan_io::parse_plan_json;
use hermes_harness::multiagent::runtime::Role;
use serde_json::{Value, json};

fn plan() -> Value {
    json!({
        "id": "ticket-1",
        "objective": "Improve tests",
        "nodes": [
            {"id": "tests", "objective": "Add regression test", "owned_paths": ["tests/new.py"]},
            {
                "id": "docs",
                "objective": "Document behavior",
                "owned_paths": ["docs/new.md"],
                "dependencies": ["tests"],
            },
        ],
    })
}

#[test]
fn accepted_plan_retains_dependencies() {
    let result = parse_plan_json(&plan().to_string()).unwrap();
    assert_eq!(result.nodes[1].dependencies, vec!["tests".to_owned()]);
    assert_eq!(result.nodes[0].role, Role::Coding);
}

#[test]
fn invalid_document_is_rejected() {
    for payload in [
        "{}",
        r#"{"id":"x","id":"y"}"#,
        r#"{"id":true,"objective":"x","nodes":[]}"#,
        r#"{"id":"x","objective":"x","nodes":[],"unauthorized":true}"#,
        "not json",
    ] {
        assert!(parse_plan_json(payload).is_err(), "{payload}");
    }
    let error = parse_plan_json(r#"{"id":"x","id":"y"}"#).unwrap_err();
    assert_eq!(error.to_string(), "Duplicate JSON key");
}

#[test]
fn unsupported_role_rejected() {
    let mut data = plan();
    data["nodes"][0]["role"] = json!("root");
    let error = parse_plan_json(&data.to_string()).unwrap_err();
    assert!(error.to_string().contains("Unknown worker role"));
}

#[test]
fn oversized_payload_rejected() {
    let error = parse_plan_json(&" ".repeat(65537)).unwrap_err();
    assert!(error.to_string().contains("byte limit"));
}

#[test]
fn ownership_overlap_rejected() {
    let mut data = plan();
    data["nodes"][1]["owned_paths"] = json!(["tests/new.py"]);
    let error = parse_plan_json(&data.to_string()).unwrap_err();
    assert!(error.to_string().contains("Overlapping"));
}

#[test]
fn plan_conversions_and_json_sources() {
    use hermes_harness::multiagent::runtime::Plan;
    use std::error::Error as _;

    let document = plan();
    assert_eq!(
        document.to_string().parse::<Plan>().unwrap(),
        Plan::try_from(&document).unwrap()
    );
    for (payload, message) in [
        ("not json", "Malformed plan JSON"),
        (r#"{"id":"x","id":"y"}"#, "Duplicate JSON key"),
    ] {
        let error = payload.parse::<Plan>().unwrap_err();
        assert_eq!(error.to_string(), message);
        assert!(error.source().unwrap().is::<serde_json::Error>());
    }
}
