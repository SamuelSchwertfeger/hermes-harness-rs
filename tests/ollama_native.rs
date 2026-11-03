#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Regression tests for Ollama-native structured inference transport.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use hermes_harness::core::ollama::{HttpResponse, Transport, TransportError};
use hermes_harness::multiagent::local_inference::{LocalEndpoint, complete_text};
use hermes_harness::multiagent::ollama_native::{
    NativeOllamaJsonTransport, OrderedJson, proposal_schema, review_schema,
};
use serde_json::{Value, json};

const EXPECTED_TIMEOUT: Duration = Duration::from_secs(30);
const EXPECTED_MAX_TOKENS: i64 = 512;

struct FakeOpener {
    native_response: Value,
    payload: RefCell<Value>,
    url: RefCell<String>,
}

impl FakeOpener {
    fn new(native_response: Value) -> Rc<Self> {
        Rc::new(Self {
            native_response,
            payload: RefCell::new(Value::Null),
            url: RefCell::new(String::new()),
        })
    }
}

impl Transport for FakeOpener {
    fn post_json(
        &self,
        url: &str,
        body: &[u8],
        timeout: Duration,
        _limit: usize,
    ) -> Result<HttpResponse, TransportError> {
        assert_eq!(timeout, EXPECTED_TIMEOUT);
        *self.url.borrow_mut() = url.to_owned();
        *self.payload.borrow_mut() = serde_json::from_slice(body).unwrap();
        Ok(HttpResponse {
            status: 200,
            body: serde_json::to_vec(&self.native_response).unwrap(),
        })
    }
}

fn endpoint(url: &str) -> LocalEndpoint {
    LocalEndpoint::new(url, "qwen3:4b")
        .with_timeout_seconds(30.0)
        .with_max_tokens(EXPECTED_MAX_TOKENS)
}

fn default_endpoint() -> LocalEndpoint {
    endpoint("http://127.0.0.1:11434/v1/chat/completions")
}

fn review_transport(opener: &Rc<FakeOpener>) -> NativeOllamaJsonTransport {
    NativeOllamaJsonTransport::new()
        .with_schema("qwen3:4b", review_schema())
        .with_opener(opener.clone())
}

#[test]
fn native_json_transport_disables_thinking_and_preserves_contract() {
    let opener = FakeOpener::new(
        json!({"message": {"content": "{\"verdict\":\"accept\",\"findings\":[]}"}}),
    );
    let text = complete_text(
        &default_endpoint(),
        "Review code; output JSON.",
        "def add(): return 2",
        &review_transport(&opener),
    )
    .unwrap();
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, json!({"verdict": "accept", "findings": []}));
    assert_eq!(*opener.url.borrow(), "http://127.0.0.1:11434/api/chat");
    let payload = opener.payload.borrow();
    assert_eq!(payload["think"], Value::Bool(false));
    assert_eq!(
        payload["format"],
        serde_json::to_value(review_schema()).unwrap()
    );
    assert_eq!(payload["options"]["num_predict"], EXPECTED_MAX_TOKENS);
    assert_eq!(payload["model"], "qwen3:4b");
}

#[test]
fn native_json_transport_rejects_empty_content() {
    let opener = FakeOpener::new(json!({"message": {"content": "", "thinking": "reasoning only"}}));
    let error = complete_text(
        &default_endpoint(),
        "system",
        "prompt",
        &review_transport(&opener),
    )
    .unwrap_err();
    assert!(error.to_string().contains("empty final content"));
}

#[test]
fn native_json_transport_rejects_invalid_response() {
    let opener = FakeOpener::new(json!({"unexpected": true}));
    let error = complete_text(
        &default_endpoint(),
        "system",
        "prompt",
        &review_transport(&opener),
    )
    .unwrap_err();
    assert!(error.to_string().contains("Invalid Ollama native response"));
}

#[test]
fn native_json_transport_keeps_loopback_guard() {
    let opener = FakeOpener::new(json!({"message": {"content": "{}"}}));
    let remote = endpoint("http://example.com/v1/chat/completions");
    let error = complete_text(&remote, "system", "prompt", &review_transport(&opener)).unwrap_err();
    assert!(error.to_string().contains("loopback"));
    assert_eq!(*opener.url.borrow(), "");
}

#[test]
fn native_json_transport_rejects_unmapped_model() {
    let opener = FakeOpener::new(json!({"message": {"content": "{}"}}));
    let transport = NativeOllamaJsonTransport::new().with_opener(opener.clone());
    let error = complete_text(&default_endpoint(), "system", "prompt", &transport).unwrap_err();
    assert!(error.to_string().contains("operator-approved JSON schema"));
    assert_eq!(*opener.url.borrow(), "");
}

fn required(schema: &hermes_harness::multiagent::ollama_native::Schema) -> Vec<String> {
    let Some(OrderedJson::Array(items)) = schema.get("required") else {
        panic!("required missing");
    };
    let mut names: Vec<String> = items
        .iter()
        .map(|item| match item {
            OrderedJson::Str(text) => text.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    names.sort();
    names
}

#[test]
fn proposal_and_review_schemas_are_distinct() {
    let proposal = proposal_schema();
    let review = review_schema();
    assert_eq!(required(&proposal), ["files", "task_id"]);
    assert_eq!(required(&review), ["findings", "verdict"]);
    assert_eq!(
        proposal.get("additionalProperties"),
        Some(&OrderedJson::Bool(false))
    );
    assert_eq!(
        review.get("additionalProperties"),
        Some(&OrderedJson::Bool(false))
    );
}

#[test]
fn rejects_invalid_bracketed_and_scoped_hosts() {
    use hermes_harness::multiagent::local_inference::Transport as _;
    for host in ["[127.0.0.1]", "x[::1]", "[::1]x", "[zz]", "[::1%eth0]"] {
        let endpoint = endpoint(&format!("http://{host}:11434/v1/chat/completions"));
        assert_eq!(
            NativeOllamaJsonTransport::new()
                .complete(&endpoint, b"{}")
                .unwrap_err()
                .to_string(),
            "Local inference requires an explicit loopback chat-completions URL"
        );
    }
}

#[test]
fn invalid_payload_retains_json_source() {
    use hermes_harness::multiagent::local_inference::Transport as _;
    use std::error::Error as _;
    let transport = NativeOllamaJsonTransport::new().with_schema("qwen3:4b", proposal_schema());
    let error = transport
        .complete(&endpoint("http://[::1]:11434/v1/chat/completions"), b"{")
        .unwrap_err();
    assert_eq!(error.to_string(), "Invalid chat request payload");
    assert!(
        error
            .source()
            .unwrap()
            .downcast_ref::<serde_json::Error>()
            .is_some()
    );
}

#[test]
fn ordered_json_conversions_preserve_wire_values() {
    assert_eq!(
        serde_json::to_string(&OrderedJson::from("text")).unwrap(),
        "\"text\""
    );
    assert_eq!(
        OrderedJson::from(String::from("text")),
        OrderedJson::from("text")
    );
    assert_eq!(
        serde_json::to_string(&OrderedJson::from(true)).unwrap(),
        "true"
    );
    assert_eq!(
        serde_json::to_string(&OrderedJson::from(false)).unwrap(),
        "false"
    );
}
