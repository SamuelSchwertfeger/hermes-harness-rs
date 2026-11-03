#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Ollama inventory parsing does not contact a server in these tests.

use hermes_harness::multiagent::ollama_discovery::parse_ollama_tags;
use serde_json::json;

const EXPECTED_MODEL_SIZE: u64 = 123;

#[test]
fn parses_ollama_models() {
    let body = json!({"models": [{"name": "local-coder:latest", "size": 123}]}).to_string();
    let models = parse_ollama_tags(body.as_bytes()).unwrap();
    assert_eq!(models[0].name, "local-coder:latest");
    assert_eq!(models[0].size_bytes, EXPECTED_MODEL_SIZE);
}

#[test]
fn handles_empty_inventory() {
    assert!(parse_ollama_tags(br#"{"models":[]}"#).unwrap().is_empty());
}

#[test]
fn rejects_invalid_inventory() {
    let values: [&[u8]; 4] = [
        b"{}",
        b"{invalid",
        br#"{"models":[{"name":"model","size":true}]}"#,
        br#"{"models":[{"name":"","size":1}]}"#,
    ];
    for value in values {
        assert!(parse_ollama_tags(value).is_err(), "{value:?}");
    }
}

#[test]
fn rejects_invalid_bracketed_and_scoped_hosts() {
    use hermes_harness::multiagent::ollama_discovery::ollama_models;
    for host in ["[127.0.0.1]", "x[::1]", "[::1]x", "[zz]", "[::1%eth0]"] {
        assert_eq!(
            ollama_models(&format!("http://{host}:11434"))
                .unwrap_err()
                .to_string(),
            "Ollama discovery endpoint must be loopback"
        );
    }
}
