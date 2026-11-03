//! Read-only Ollama discovery on the operator's loopback endpoint.
//!
//! No model pull, load, unload, or remote inference is performed by discovery.

use std::io::Read as _;
use std::time::Duration;

use serde_json::Value;

use super::local_inference::{LocalInferenceError, SplitUrl};
use crate::core::ollama::{HttpResponse, TransportError};
use crate::util::is_py_space;

/// Largest accepted `/api/tags` body in bytes.
pub const MAX_TAGS_RESPONSE: usize = 64 * 1024;
/// Most models a listing may contain.
pub const MAX_OLLAMA_MODELS: usize = 256;
/// Longest model name in characters.
pub const MAX_MODEL_NAME_LENGTH: usize = 200;
/// The only accepted HTTP status.
pub const HTTP_OK: u16 = 200;
/// Default Ollama endpoint.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";

/// One installed model as reported by Ollama.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OllamaModel {
    /// Model name, e.g. `qwen3:4b`.
    pub name: String,
    /// Size on disk in bytes.
    pub size_bytes: u64,
}

impl OllamaModel {
    /// Model with the given name and size.
    #[must_use]
    pub fn new(name: &str, size_bytes: u64) -> Self {
        Self {
            name: name.to_owned(),
            size_bytes,
        }
    }
}

fn fail<T>(message: &str) -> Result<T, LocalInferenceError> {
    Err(LocalInferenceError::msg(message))
}

fn parse_model(item: &Value) -> Result<OllamaModel, LocalInferenceError> {
    let Some(entry) = item.as_object() else {
        return fail("Malformed Ollama model entry");
    };
    let name = entry.get("name").and_then(Value::as_str).filter(|name| {
        !name.chars().all(is_py_space) && name.chars().count() <= MAX_MODEL_NAME_LENGTH
    });
    let Some(name) = name else {
        return fail("Invalid Ollama model name");
    };
    let Some(size) = entry.get("size").and_then(Value::as_u64) else {
        return fail("Invalid Ollama model size");
    };
    Ok(OllamaModel::new(name, size))
}

/// Admit bounded Ollama `/api/tags` output without trusting arbitrary fields.
///
/// # Errors
///
/// Returns [`LocalInferenceError`] for an oversized, malformed or wrongly shaped listing, too
/// many models, or an invalid model entry.
pub fn parse_ollama_tags(data: &[u8]) -> Result<Vec<OllamaModel>, LocalInferenceError> {
    if data.len() > MAX_TAGS_RESPONSE {
        return fail("Ollama model listing exceeds limit");
    }
    let document: Value = serde_json::from_slice(data)
        .map_err(|error| LocalInferenceError::caused("Malformed Ollama model listing", error))?;
    let Some(rows) = document.get("models").and_then(Value::as_array) else {
        return fail("Invalid Ollama model listing");
    };
    if rows.len() > MAX_OLLAMA_MODELS {
        return fail("Too many Ollama models");
    }
    rows.iter().map(parse_model).collect()
}

/// Injectable HTTP GET so tests stay offline.
pub trait HttpGet {
    /// GET `url` and return the status plus at most `limit` body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] for connection-level failures and HTTP error statuses.
    fn get(
        &self,
        url: &str,
        timeout: Duration,
        limit: usize,
    ) -> Result<HttpResponse, TransportError>;
}

/// Real GET over `ureq`: no proxy, no redirects.
#[derive(Debug, Clone, Copy, Default)]
pub struct UreqGet;

impl HttpGet for UreqGet {
    fn get(
        &self,
        url: &str,
        timeout: Duration,
        limit: usize,
    ) -> Result<HttpResponse, TransportError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .proxy(None)
            .timeout_connect(Some(timeout))
            // Deviation: ureq 3 has no per-read timeout; each phase gets `timeout`.
            .timeout_recv_response(Some(timeout))
            .timeout_recv_body(Some(timeout))
            .timeout_send_request(Some(timeout))
            .timeout_send_body(Some(timeout))
            .max_redirects(0)
            .build()
            .into();
        let response = agent.get(url).call().map_err(TransportError::from)?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(u64::try_from(limit).unwrap_or(u64::MAX))
            .read_to_end(&mut body)
            .map_err(TransportError::from)?;
        Ok(HttpResponse { status, body })
    }
}

/// Fetch model inventory only from explicit literal loopback endpoints.
///
/// # Errors
///
/// Returns [`LocalInferenceError`] for a non-loopback endpoint, an unreachable or failing
/// server, or an invalid listing.
pub fn ollama_models(base_url: &str) -> Result<Vec<OllamaModel>, LocalInferenceError> {
    ollama_models_with(&UreqGet, base_url)
}

/// [`ollama_models`] over an injected HTTP client.
///
/// # Errors
///
/// Same as [`ollama_models`].
pub fn ollama_models_with(
    http: &dyn HttpGet,
    base_url: &str,
) -> Result<Vec<OllamaModel>, LocalInferenceError> {
    let allowed = SplitUrl::parse(base_url).is_some_and(|url| {
        url.scheme == "http"
            && url.is_loopback()
            && matches!(url.path.as_str(), "" | "/")
            && url.query.is_empty()
            && url.fragment.is_empty()
            && !url.has_userinfo
    });
    if !allowed {
        return fail("Ollama discovery endpoint must be loopback");
    }
    let url = format!("{}/api/tags", base_url.trim_end_matches('/'));
    let unavailable = "Ollama model inventory unavailable";
    let response = http
        .get(&url, Duration::from_secs(5), MAX_TAGS_RESPONSE + 1)
        .map_err(|error| LocalInferenceError::caused(unavailable, error))?;
    if (300..400).contains(&response.status) {
        return fail(unavailable);
    }
    if response.status != HTTP_OK {
        return fail("Ollama inventory request failed");
    }
    parse_ollama_tags(&response.body)
}
