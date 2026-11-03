#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline deterministic transport tests for the local model adapter.

use hermes_harness::multiagent::local_inference::{
    LocalEndpoint, LocalInferenceError, Transport, complete_text, propose_edits, propose_plan,
    propose_replacements,
};
use hermes_harness::util::ordered_map::OrderedMap;
use hermes_harness::util::sha256_hex;
use serde_json::{Value, json};

struct FakeTransport {
    text: String,
}

impl FakeTransport {
    fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
        }
    }
}

impl Transport for FakeTransport {
    fn complete(
        &self,
        endpoint: &LocalEndpoint,
        payload: &[u8],
    ) -> Result<Vec<u8>, LocalInferenceError> {
        let data: Value = serde_json::from_slice(payload).unwrap();
        assert_eq!(data["model"], endpoint.model);
        assert_eq!(data["stream"], Value::Bool(false));
        Ok(serde_json::to_vec(&json!({"choices": [{"message": {"content": self.text}}]})).unwrap())
    }
}

const CHAT: &str = "http://127.0.0.1:8080/v1/chat/completions";
const OLLAMA_CHAT: &str = "http://127.0.0.1:11434/v1/chat/completions";

fn owned(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| (*p).to_owned()).collect()
}

#[test]
fn accepts_structured_local_plan() {
    let plan = json!({
        "id": "ticket",
        "objective": "Add test",
        "nodes": [{"id": "test", "objective": "Write a test", "owned_paths": ["tests/x.py"]}],
    })
    .to_string();
    let output = propose_plan(
        &LocalEndpoint::new(CHAT, "configured-model"),
        "Create a test",
        &FakeTransport::new(&plan),
    )
    .unwrap();
    assert_eq!(output.nodes[0].id, "test");
}

#[test]
fn rejects_unqualified_endpoint() {
    for url in [
        "https://127.0.0.1/v1/chat/completions",
        "http://example.com/v1/chat/completions",
        "http://localhost/v1/chat/completions",
        "http://127.0.0.1:8080/other",
        "http://127.0.0.1:8080/v1/chat/completions?token=x",
    ] {
        let error = LocalEndpoint::new(url, "test").validate().unwrap_err();
        assert!(error.to_string().contains("loopback"), "{url}: {error}");
    }
}

#[test]
fn rejects_unstructured_response() {
    let endpoint = LocalEndpoint::new(CHAT, "test");
    let error =
        propose_plan(&endpoint, "create plan", &FakeTransport::new("not json")).unwrap_err();
    assert!(error.to_string().contains("plan admission"));
}

#[test]
fn budget_and_response_contract() {
    let endpoint = LocalEndpoint::new(CHAT, "test");
    assert_eq!(
        complete_text(&endpoint, "system", "prompt", &FakeTransport::new("ok")).unwrap(),
        "ok"
    );
    let error = complete_text(
        &endpoint,
        "system",
        &"x".repeat(20_000),
        &FakeTransport::new("ok"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("oversized"));
    let error = LocalEndpoint::new(&endpoint.url, "test")
        .with_max_tokens(10_000)
        .validate()
        .unwrap_err();
    assert!(error.to_string().contains("budget"));
}

#[test]
fn local_model_returns_scoped_file_proposal() {
    let endpoint = LocalEndpoint::new(CHAT, "test");
    let body = json!({
        "task_id": "ticket-a",
        "files": [{"path": "tests/new.py", "content": "assert True\\n"}],
    })
    .to_string();
    let result = propose_edits(
        &endpoint,
        "ticket-a",
        "Add test",
        &owned(&["tests"]),
        &FakeTransport::new(&body),
    )
    .unwrap();
    assert_eq!(result.files[0].path, "tests/new.py");
}

#[test]
fn local_model_unowned_proposal_rejected() {
    let endpoint = LocalEndpoint::new(CHAT, "test");
    let body = json!({
        "task_id": "ticket-a",
        "files": [{"path": "outside.py", "content": "bad"}],
    })
    .to_string();
    let error = propose_edits(
        &endpoint,
        "ticket-a",
        "Add test",
        &owned(&["tests"]),
        &FakeTransport::new(&body),
    )
    .unwrap_err();
    assert!(error.to_string().contains("ownership admission"));
}

#[test]
fn local_replacement_echoes_supplied_preimage() {
    let endpoint = LocalEndpoint::new(OLLAMA_CHAT, "coder");
    let digest = sha256_hex(b"original");
    let response = json!({
        "task_id": "task-a",
        "replacements": [{
            "path": "src/code.py",
            "expected_sha256": digest,
            "content": "updated = True\\n",
        }],
    })
    .to_string();
    let preimages: OrderedMap<String> = [("src/code.py".to_owned(), digest.clone())]
        .into_iter()
        .collect();
    let result = propose_replacements(
        &endpoint,
        "task-a",
        "Update code",
        &owned(&["src"]),
        &preimages,
        &FakeTransport::new(&response),
    )
    .unwrap();
    assert_eq!(result[0].expected_sha256, digest);
}

#[test]
fn local_replacement_rejects_different_preimage() {
    let endpoint = LocalEndpoint::new(OLLAMA_CHAT, "coder");
    let response = json!({
        "task_id": "task-a",
        "replacements": [{
            "path": "src/code.py",
            "expected_sha256": "b".repeat(64),
            "content": "unsafe",
        }],
    })
    .to_string();
    let preimages: OrderedMap<String> = [("src/code.py".to_owned(), "a".repeat(64))]
        .into_iter()
        .collect();
    let error = propose_replacements(
        &endpoint,
        "task-a",
        "Update code",
        &owned(&["src"]),
        &preimages,
        &FakeTransport::new(&response),
    )
    .unwrap_err();
    assert!(error.to_string().contains("does not match"));
}

#[test]
fn rejects_invalid_bracketed_and_scoped_hosts() {
    for host in ["[127.0.0.1]", "x[::1]", "[::1]x", "[zz]", "[::1%eth0]"] {
        let endpoint =
            LocalEndpoint::new(&format!("http://{host}:11434/v1/chat/completions"), "model");
        assert_eq!(
            endpoint.validate().unwrap_err().to_string(),
            "Local inference requires an explicit loopback chat-completions URL"
        );
    }
    LocalEndpoint::new("http://[::1]:11434/v1/chat/completions", "model")
        .validate()
        .unwrap();
}
