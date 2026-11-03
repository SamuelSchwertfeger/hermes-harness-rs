//! Operator-approved on-demand inference from any installed model on the XPS Ollama pool
//! (upstream naming), reached through a local tunnel.

use std::rc::Rc;

use serde::Serialize;
use serde_json::Value;

use super::local_inference::{LocalEndpoint, LocalInferenceError, complete_text};
use super::ollama_discovery::{HttpGet, UreqGet, ollama_models_with};
use super::ollama_native::{NativeOllamaJsonTransport, OrderedJson, Schema};
use crate::core::ollama::Transport as HttpTransport;
use crate::util::ordered_map::OrderedMap;

/// The only approved XPS pool endpoint (a local tunnel).
pub const XPS_BASE: &str = "http://127.0.0.1:11435";

const MAX_MODEL_ID_CHARS: usize = 128;

fn fail<T>(message: &str) -> Result<T, LocalInferenceError> {
    Err(LocalInferenceError::msg(message))
}

/// Python `MODEL_ID.fullmatch`: `[a-zA-Z0-9_][a-zA-Z0-9_.:/-]{0,127}`.
fn is_model_id(name: &str) -> bool {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    first_ok
        && name.chars().count() <= MAX_MODEL_ID_CHARS
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-'))
}

fn response_schema() -> Schema {
    let string_type = || {
        OrderedJson::Object(OrderedMap::from_iter([(
            "type".to_owned(),
            OrderedJson::Str("string".to_owned()),
        )]))
    };
    Schema::from_iter([
        ("type".to_owned(), OrderedJson::Str("object".to_owned())),
        (
            "properties".to_owned(),
            OrderedJson::Object(OrderedMap::from_iter([(
                "response".to_owned(),
                string_type(),
            )])),
        ),
        (
            "required".to_owned(),
            OrderedJson::Array(vec![OrderedJson::Str("response".to_owned())]),
        ),
        ("additionalProperties".to_owned(), OrderedJson::Bool(false)),
    ])
}

/// Discover installed models live, with no model downloads.
///
/// # Errors
///
/// Returns [`LocalInferenceError`] for an unapproved endpoint or a failed discovery.
pub fn available_models(base_url: &str) -> Result<Vec<String>, LocalInferenceError> {
    available_models_with(&UreqGet, base_url)
}

/// [`available_models`] over an injected HTTP client.
///
/// # Errors
///
/// Same as [`available_models`].
pub fn available_models_with(
    http: &dyn HttpGet,
    base_url: &str,
) -> Result<Vec<String>, LocalInferenceError> {
    if base_url != XPS_BASE {
        return fail("Unapproved XPS inference endpoint");
    }
    Ok(ollama_models_with(http, base_url)?
        .into_iter()
        .map(|model| model.name)
        .filter(|name| is_model_id(name) && name.split('/').all(|p| !matches!(p, "" | "." | "..")))
        .collect())
}

/// Result of one authorised inference (JSON field order is the Python dict order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Inference {
    /// Model that answered.
    pub model: String,
    /// The model's `response` string.
    pub response: String,
    /// Always false: model-issued commands are never run.
    pub executed_tools: bool,
}

/// Infer using any currently installed model; never run model-issued commands.
///
/// # Errors
///
/// Returns [`LocalInferenceError`] without authorization, for an unavailable model, a failed
/// inference, or a response that is not exactly `{"response": <string>}`.
pub fn infer(
    model: &str,
    prompt: &str,
    authorized: bool,
) -> Result<Inference, LocalInferenceError> {
    infer_with(&UreqGet, None, model, prompt, authorized)
}

/// [`infer`] over an injected inventory client and chat client (`None` uses `ureq`).
///
/// # Errors
///
/// Same as [`infer`].
pub fn infer_with(
    http: &dyn HttpGet,
    chat: Option<Rc<dyn HttpTransport>>,
    model: &str,
    prompt: &str,
    authorized: bool,
) -> Result<Inference, LocalInferenceError> {
    if !authorized {
        return fail("XPS inference needs operator authorization");
    }
    if !available_models_with(http, XPS_BASE)?
        .iter()
        .any(|name| name == model)
    {
        return fail("Requested XPS model unavailable");
    }
    let endpoint = LocalEndpoint::new(&format!("{XPS_BASE}/v1/chat/completions"), model)
        .with_timeout_seconds(120.0)
        .with_max_tokens(512);
    let mut transport = NativeOllamaJsonTransport::new().with_schema(model, response_schema());
    if let Some(chat) = chat {
        transport = transport.with_opener(chat);
    }
    let result = complete_text(
        &endpoint,
        "Return one JSON object containing response as a string. \
Do not claim tool execution or file edits.",
        prompt,
        &transport,
    )?;
    let parsed: Value = serde_json::from_str(&result)
        .map_err(|error| LocalInferenceError::caused("Invalid XPS response JSON", error))?;
    let response = parsed
        .as_object()
        .filter(|map| map.len() == 1)
        .and_then(|map| map.get("response"))
        .and_then(Value::as_str);
    let Some(response) = response else {
        return fail("Invalid XPS response schema");
    };
    Ok(Inference {
        model: model.to_owned(),
        response: response.to_owned(),
        executed_tools: false,
    })
}
