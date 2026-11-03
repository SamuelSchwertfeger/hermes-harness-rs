#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Hermes-facing API regression tests for offline-only operation.

use std::path::Path;

use hermes_harness::multiagent::local_inference::{LocalEndpoint, LocalInferenceError, Transport};
use hermes_harness::multiagent::runtime::{Node, Plan, Role};
use hermes_harness::native::workflows::{
    NativeContext, admit_plan_json, agents_assemble_offline, game_plan, propose_local_edits,
};
use serde_json::json;

fn context(tmp: &Path) -> NativeContext {
    let repo = tmp.join("repo");
    let state = tmp.join("state");
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&state).unwrap();
    NativeContext::new("hermes-session", repo, state)
}

fn plan() -> Plan {
    Plan::new(
        "ticket",
        "Task",
        vec![Node::new("worker", "Write test", &["tests/new.py"])],
    )
}

#[test]
fn native_plan_echoes_session() {
    let tmp = tempfile::tempdir().unwrap();
    let result = game_plan(&context(tmp.path()), &plan()).unwrap();
    assert_eq!(result["session_id"], "hermes-session");
    assert_eq!(result["ordered_nodes"], json!(["worker"]));
    assert_eq!(result["mode"], "validation-only");
}

#[test]
fn admit_model_plan_before_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let payload = json!({
        "id": "ticket",
        "objective": "Task",
        "nodes": [{"id": "worker", "objective": "Write test", "owned_paths": ["tests/new.py"]}],
    });
    let result = admit_plan_json(&context(tmp.path()), &payload.to_string()).unwrap();
    assert_eq!(result["plan_id"], "ticket");
    assert!(!tmp.path().join("state/ticket").exists());
}

#[test]
fn offline_dispatch_requires_explicit_authorization() {
    let tmp = tempfile::tempdir().unwrap();
    let error = agents_assemble_offline(&context(tmp.path()), &plan(), false).unwrap_err();
    assert!(
        error.to_string().contains("Explicit offline authorization"),
        "{error}"
    );
    assert!(!tmp.path().join("state/ticket").exists());
}

#[test]
fn native_cyber_dispatch_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = Plan::new(
        "ticket",
        "Task",
        vec![Node::new("worker", "Review", &["tests/new.py"]).with_role(Role::CyberWarrior)],
    );
    let error = agents_assemble_offline(&context(tmp.path()), &plan, true).unwrap_err();
    assert!(error.to_string().contains("grant transport"), "{error}");
    assert!(!tmp.path().join("state/ticket").exists());
}

#[test]
fn invalid_session_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let base = context(tmp.path());
    let blank = NativeContext::new(" ", base.repository, base.state_root);
    let error = game_plan(&blank, &plan()).unwrap_err();
    assert!(error.to_string().contains("Invalid originating"), "{error}");
}

#[test]
fn native_local_proposal_requires_explicit_grant() {
    struct NoTransport;

    impl Transport for NoTransport {
        fn complete(
            &self,
            _endpoint: &LocalEndpoint,
            _payload: &[u8],
        ) -> Result<Vec<u8>, LocalInferenceError> {
            panic!("No model calls permitted without authorization");
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let endpoint = LocalEndpoint::new("http://127.0.0.1:8080/v1/chat/completions", "test");
    let node = plan().nodes.remove(0);
    let error = propose_local_edits(&context(tmp.path()), &endpoint, &node, &NoTransport, false)
        .unwrap_err();
    assert!(
        error.to_string().contains("Explicit local inference"),
        "{error}"
    );
}
