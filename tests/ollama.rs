#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Test the real response decoding path without live inference.

use std::time::Duration;

use hermes_harness::core::ollama::{
    HttpResponse, OllamaError, OllamaOptions, Transport, TransportError, text_task_with,
};
use hermes_harness::core::routing::Role;

struct FakeTransport;

impl Transport for FakeTransport {
    fn post_json(
        &self,
        _url: &str,
        _body: &[u8],
        _timeout: Duration,
        _limit: usize,
    ) -> Result<HttpResponse, TransportError> {
        Ok(HttpResponse {
            status: 200,
            body: serde_json::to_vec(&serde_json::json!({"response": "ok"})).unwrap(),
        })
    }
}

#[test]
fn ollama_body_read_once() {
    let dir = tempfile::tempdir().unwrap();
    let options = OllamaOptions {
        lock_file: dir.path().join("gpu.lock"),
        ..OllamaOptions::default()
    };
    assert_eq!(
        text_task_with(&FakeTransport, "hello", Role::Scout, Some(&options)).unwrap(),
        "ok"
    );
}

#[test]
fn remote_endpoint_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let options = OllamaOptions {
        url: "http://example.com/api/generate".to_owned(),
        lock_file: dir.path().join("gpu.lock"),
    };
    let err = text_task_with(&FakeTransport, "hello", Role::Scout, Some(&options)).unwrap_err();
    assert!(matches!(err, OllamaError::Denied(_)));
    assert!(err.to_string().contains("loopback"));
}

struct FailingTransport;

impl Transport for FailingTransport {
    fn post_json(
        &self,
        _url: &str,
        _body: &[u8],
        _timeout: Duration,
        _limit: usize,
    ) -> Result<HttpResponse, TransportError> {
        Err(std::io::Error::other("injected transport failure").into())
    }
}

#[test]
fn transport_failure_preserves_message_and_source_chain() {
    use std::error::Error as _;
    let dir = tempfile::tempdir().unwrap();
    let options = OllamaOptions {
        lock_file: dir.path().join("gpu.lock"),
        ..OllamaOptions::default()
    };
    let error =
        text_task_with(&FailingTransport, "hello", Role::Scout, Some(&options)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Ollama request failed; verify idle state before retry"
    );
    assert!(matches!(error, OllamaError::Transport(_)));
    let transport = error.source().unwrap();
    let cause = transport.source().unwrap();
    assert_eq!(cause.to_string(), "injected transport failure");
    assert!(cause.downcast_ref::<std::io::Error>().is_some());
}
