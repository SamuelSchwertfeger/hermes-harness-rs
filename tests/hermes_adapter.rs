#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! No real gateway, Ollama server or Codex client is started in these tests.

use std::fs;
use std::path::Path;

use hermes_harness::gateway::hermes_adapter::{
    HermesAdapterError, TaskKind, TaskRequest, execute, prepare,
};

fn request(tmp: &Path, kind: TaskKind) -> TaskRequest {
    TaskRequest::new("m2-001", kind, "Review this test", tmp)
}

#[test]
fn granite_route_is_disabled_for_execution() {
    let tmp = tempfile::tempdir().unwrap();
    let prepared = prepare(&request(tmp.path(), TaskKind::Scout)).unwrap();
    assert_eq!(
        (prepared.provider.as_str(), prepared.model.as_str()),
        ("ollama", "granite4:3b")
    );
    assert!(!prepared.execution_enabled);
    let error = execute(&prepared).unwrap_err();
    assert!(matches!(error, HermesAdapterError::Safety(_)));
    assert!(error.to_string().contains("execution disabled"));
}

#[test]
fn codex_plan_route() {
    let tmp = tempfile::tempdir().unwrap();
    let prepared = prepare(&request(tmp.path(), TaskKind::Plan)).unwrap();
    assert_eq!(
        (prepared.provider.as_str(), prepared.model.as_str()),
        ("openai-codex", "gpt-6.1-sol")
    );
}

#[test]
fn scope_and_cyber_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let error = prepare(&request(tmp.path(), TaskKind::Cyber)).unwrap_err();
    assert!(matches!(error, HermesAdapterError::Permission(_)));
    assert!(error.to_string().contains("authorized scope"));
    let mut scoped = TaskRequest::new(
        "m2-001",
        TaskKind::Cyber,
        "Offline defensive review",
        tmp.path(),
    );
    scoped.authorized_scope = Some("local-test-fixtures".to_owned());
    let error = prepare(&scoped).unwrap_err();
    assert!(matches!(error, HermesAdapterError::Safety(_)));
    assert!(error.to_string().contains("Cyber adapter disabled"));
}

#[test]
fn budget_and_workspace_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut invalid = TaskRequest::new("m2-001", TaskKind::Scout, "Review", tmp.path());
    invalid.max_turns = 0;
    let error = prepare(&invalid).unwrap_err();
    assert!(matches!(error, HermesAdapterError::Value(_)));
    assert!(error.to_string().contains("budgets"));
}

#[test]
fn private_manifest_no_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let prepared = prepare(&request(tmp.path(), TaskKind::Scout)).unwrap();
    let destination = tmp.path().join("runs").join("request.json");
    prepared.save(&destination).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&destination).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let saved: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&destination).unwrap()).unwrap();
    assert_eq!(saved["execution_enabled"], serde_json::Value::Bool(false));
    let error = prepared.save(&destination).unwrap_err();
    assert!(matches!(error, HermesAdapterError::FileExists(_)));
    assert!(error.to_string().contains("overwrite"));
}
