//! Bounded Ollama-native structured JSON adapter for the local inference protocol.

use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::local_inference::{
    HTTP_OK, LocalEndpoint, LocalInferenceError, MAX_RESPONSE_BYTES, SplitUrl, Transport,
};
use crate::core::ollama::{Transport as HttpTransport, UreqTransport};
use crate::util::ordered_map::OrderedMap;
use crate::util::{is_py_space, ordered_json_spaced};

/// JSON value that keeps object key order, like a Python `dict` read by `json.loads`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OrderedJson {
    /// JSON `null`.
    Null,
    /// JSON boolean.
    Bool(bool),
    /// Signed integer.
    Int(i64),
    /// Unsigned integer beyond the signed range.
    Uint(u64),
    /// Floating point number.
    Float(f64),
    /// String.
    Str(String),
    /// Array.
    Array(Vec<Self>),
    /// Object in insertion order.
    Object(OrderedMap<Self>),
}

impl From<String> for OrderedJson {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<&str> for OrderedJson {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<bool> for OrderedJson {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl OrderedJson {
    /// Member `key` when this value is an object that has it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(map) => map.get(key),
            _ => None,
        }
    }

    fn string(text: &str) -> Self {
        Self::Str(text.to_owned())
    }

    fn object<const N: usize>(entries: [(&str, Self); N]) -> Self {
        Self::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    fn strings<const N: usize>(items: [&str; N]) -> Self {
        Self::Array(items.into_iter().map(Self::string).collect())
    }
}

/// Operator-approved JSON schema sent as the Ollama `format` field.
pub type Schema = OrderedMap<OrderedJson>;

fn into_schema(value: OrderedJson) -> Schema {
    match value {
        OrderedJson::Object(map) => map,
        _ => Schema::new(),
    }
}

/// `{"type": "string"}` schema property.
pub(crate) fn string_property() -> OrderedJson {
    OrderedJson::object([("type", OrderedJson::string("string"))])
}

/// Closed object schema: `type`, `properties`, `required`, `additionalProperties: false`.
pub(crate) fn closed_object<const N: usize, const M: usize>(
    properties: [(&str, OrderedJson); N],
    required: [&str; M],
) -> OrderedJson {
    OrderedJson::object([
        ("type", OrderedJson::string("object")),
        ("properties", OrderedJson::object(properties)),
        ("required", OrderedJson::strings(required)),
        ("additionalProperties", OrderedJson::Bool(false)),
    ])
}

/// Array schema with an optional `minItems`, a `maxItems` bound and an item schema.
pub(crate) fn bounded_array(
    min_items: Option<i64>,
    max_items: i64,
    items: OrderedJson,
) -> OrderedJson {
    let mut entries = vec![("type", OrderedJson::string("array"))];
    if let Some(min) = min_items {
        entries.push(("minItems", OrderedJson::Int(min)));
    }
    entries.push(("maxItems", OrderedJson::Int(max_items)));
    entries.push(("items", items));
    OrderedJson::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

/// Shared task envelope: `task_id` plus a 1..=16 element list of `item` under `list_key`.
pub(crate) fn task_proposal_schema(list_key: &str, item: OrderedJson) -> Schema {
    into_schema(closed_object(
        [
            ("task_id", string_property()),
            (list_key, bounded_array(Some(1), 16, item)),
        ],
        ["task_id", list_key],
    ))
}

/// Wire schema for file proposals (Python `PROPOSAL_SCHEMA`).
#[must_use]
pub fn proposal_schema() -> Schema {
    task_proposal_schema(
        "files",
        closed_object(
            [("path", string_property()), ("content", string_property())],
            ["path", "content"],
        ),
    )
}

/// Wire schema for reviewer verdicts (Python `REVIEW_SCHEMA`).
#[must_use]
pub fn review_schema() -> Schema {
    into_schema(closed_object(
        [
            (
                "verdict",
                OrderedJson::object([
                    ("type", OrderedJson::string("string")),
                    ("enum", OrderedJson::strings(["accept", "changes_required"])),
                ]),
            ),
            ("findings", bounded_array(None, 20, string_property())),
        ],
        ["verdict", "findings"],
    ))
}

fn fail<T>(message: &str) -> Result<T, LocalInferenceError> {
    Err(LocalInferenceError::msg(message))
}

fn native_url(endpoint: &LocalEndpoint) -> Result<String, LocalInferenceError> {
    endpoint.validate()?;
    let Some(parsed) = SplitUrl::parse(&endpoint.url) else {
        return fail("Ollama-native transport requires loopback");
    };
    if !parsed.is_loopback() {
        return fail("Ollama-native transport requires loopback");
    }
    let address = if parsed.hostname.as_deref() == Some("::1") {
        "[::1]"
    } else {
        "127.0.0.1"
    };
    let port = parsed.port.map_err(LocalInferenceError::Message)?;
    let port = port.filter(|value| *value != 0).unwrap_or(80);
    Ok(format!("http://{address}:{port}/api/chat"))
}

#[derive(Serialize)]
struct NativeOptions {
    temperature: f64,
    num_predict: i64,
}

#[derive(Serialize)]
struct NativeRequest<'a> {
    model: &'a str,
    stream: bool,
    think: bool,
    format: &'a Schema,
    options: NativeOptions,
    messages: &'a OrderedJson,
}

fn native_payload(
    endpoint: &LocalEndpoint,
    payload: &[u8],
    schema: &Schema,
    think: bool,
    temperature: f64,
) -> Result<String, LocalInferenceError> {
    let request = serde_json::from_slice::<OrderedJson>(payload)
        .map_err(|error| LocalInferenceError::caused("Invalid chat request payload", error))?;
    let valid = request.get("model") == Some(&OrderedJson::Str(endpoint.model.clone()))
        && request.get("stream") == Some(&OrderedJson::Bool(false));
    let messages = request.get("messages");
    match messages {
        Some(messages @ OrderedJson::Array(_))
            if valid && matches!(request, OrderedJson::Object(_)) =>
        {
            Ok(ordered_json_spaced(&NativeRequest {
                model: &endpoint.model,
                stream: false,
                think,
                format: schema,
                options: NativeOptions {
                    temperature,
                    num_predict: endpoint.max_tokens,
                },
                messages,
            })?)
        }
        _ => fail("Invalid chat request contract"),
    }
}

fn read_native_response(
    transport: &dyn HttpTransport,
    url: &str,
    body: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, LocalInferenceError> {
    let request_failed = "Ollama native inference request failed";
    let response = transport
        .post_json(url, body, timeout, MAX_RESPONSE_BYTES + 1)
        .map_err(|error| LocalInferenceError::caused(request_failed, error))?;
    if (300..400).contains(&response.status) {
        return fail(request_failed);
    }
    if response.status != HTTP_OK {
        return fail("Ollama returned unsuccessful HTTP status");
    }
    if response.body.len() > MAX_RESPONSE_BYTES {
        return fail("Ollama response exceeds limit");
    }
    Ok(response.body)
}

#[derive(Serialize)]
struct Content<'a> {
    content: &'a str,
}

#[derive(Serialize)]
struct Choice<'a> {
    message: Content<'a>,
}

#[derive(Serialize)]
struct Completion<'a> {
    choices: [Choice<'a>; 1],
}

/// Decode a native response without issuing an HTTP request.
///
/// # Errors
/// Returns the same validation errors as the native response converter.
#[doc(hidden)]
pub fn decode_chat_completion_for_fuzzing(raw: &[u8]) -> Result<Vec<u8>, LocalInferenceError> {
    as_chat_completion(raw)
}

fn as_chat_completion(raw: &[u8]) -> Result<Vec<u8>, LocalInferenceError> {
    let document: Value = serde_json::from_slice(raw)
        .map_err(|error| LocalInferenceError::caused("Malformed Ollama native response", error))?;
    let Some(message) = document
        .get("message")
        .filter(|message| message.is_object())
    else {
        return fail("Invalid Ollama native response");
    };
    let Some(content) = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.chars().all(is_py_space))
    else {
        return fail("Ollama returned empty final content");
    };
    let result = ordered_json_spaced(&Completion {
        choices: [Choice {
            message: Content { content },
        }],
    })?
    .into_bytes();
    if result.len() > MAX_RESPONSE_BYTES {
        return fail("Converted Ollama response exceeds limit");
    }
    Ok(result)
}

/// Adapt allowlisted chat requests to Ollama `/api/chat`, without tools or redirects.
#[derive(Clone, Default)]
pub struct NativeOllamaJsonTransport {
    /// Request model thinking (Python `think`).
    pub think: bool,
    /// Sampling temperature.
    pub temperature: f64,
    /// Explicit mapping prevents inferring task roles from untrusted model prompts.
    pub schemas_by_model: BTreeMap<String, Schema>,
    opener: Option<Rc<dyn HttpTransport>>,
}

impl fmt::Debug for NativeOllamaJsonTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeOllamaJsonTransport")
            .field("think", &self.think)
            .field("temperature", &self.temperature)
            .field("schemas_by_model", &self.schemas_by_model)
            .finish_non_exhaustive()
    }
}

impl NativeOllamaJsonTransport {
    /// Transport with no approved schemas, thinking off and temperature zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Approve `schema` for `model`.
    #[must_use]
    pub fn with_schema(mut self, model: &str, schema: Schema) -> Self {
        self.schemas_by_model.insert(model.to_owned(), schema);
        self
    }

    /// Replace the HTTP transport used for inference requests.
    #[must_use]
    pub fn with_opener(mut self, opener: Rc<dyn HttpTransport>) -> Self {
        self.opener = Some(opener);
        self
    }
}

impl Transport for NativeOllamaJsonTransport {
    /// Return the original pipeline's one-choice response representation.
    fn complete(
        &self,
        endpoint: &LocalEndpoint,
        payload: &[u8],
    ) -> Result<Vec<u8>, LocalInferenceError> {
        let url = native_url(endpoint)?;
        let Some(schema) = self.schemas_by_model.get(&endpoint.model) else {
            return fail("No operator-approved JSON schema for local model");
        };
        let body = native_payload(endpoint, payload, schema, self.think, self.temperature)?;
        let default = UreqTransport;
        let transport: &dyn HttpTransport = self.opener.as_deref().unwrap_or(&default);
        let timeout = Duration::try_from_secs_f64(endpoint.timeout_seconds).map_err(|error| {
            LocalInferenceError::caused("Ollama native inference request failed", error)
        })?;
        let raw = read_native_response(transport, &url, body.as_bytes(), timeout)?;
        as_chat_completion(&raw)
    }
}

#[cfg(test)]
mod schema_tests {
    #![allow(clippy::unwrap_used)]

    #[test]
    fn proposal_schema_snapshot() {
        let text = crate::util::ordered_json_spaced(&super::proposal_schema()).unwrap();
        assert_eq!(
            text,
            r#"{"type": "object", "properties": {"task_id": {"type": "string"}, "files": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"], "additionalProperties": false}}}, "required": ["task_id", "files"], "additionalProperties": false}"#
        );
    }

    #[test]
    fn review_schema_snapshot() {
        let text = crate::util::ordered_json_spaced(&super::review_schema()).unwrap();
        assert_eq!(
            text,
            r#"{"type": "object", "properties": {"verdict": {"type": "string", "enum": ["accept", "changes_required"]}, "findings": {"type": "array", "maxItems": 20, "items": {"type": "string"}}}, "required": ["verdict", "findings"], "additionalProperties": false}"#
        );
    }
}
