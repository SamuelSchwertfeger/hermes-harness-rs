//! Bounded opt-in local chat-completions inference; no tool execution.
//!
//! This is NOT wired to the offline worker factory. Responses are untrusted text
//! and must pass the canonical structured-plan admission boundary.

use std::error::Error;
use std::net::Ipv6Addr;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use super::model_edits::{EditProposal, parse_edit_proposal};
use super::plan_io::parse_plan_json;
use super::replace_edits::FileReplacement;
use super::replacement_proposals::parse_replacement_proposal;
use super::runtime::Plan;
use crate::core::ollama::{Transport as HttpTransport, UreqTransport};
use crate::multiagent::model_edits::is_owned;
use crate::util::ordered_map::OrderedMap;
use crate::util::{JsonError, is_py_space, ordered_json_spaced};

/// Largest accepted response body in bytes.
pub const MAX_RESPONSE_BYTES: usize = 256 * 1024;
/// Largest system plus user prompt in characters.
pub const MAX_PROMPT_CHARS: usize = 16_384;
/// Longest model identifier in characters.
pub const MAX_MODEL_ID_LENGTH: usize = 128;
/// Longest allowed request timeout in seconds.
pub const MAX_INFERENCE_SECONDS: u32 = 120;
/// Largest allowed completion budget in tokens.
pub const MAX_INFERENCE_TOKENS: i64 = 8192;
/// The only accepted HTTP status.
pub const HTTP_OK: u16 = 200;
/// Most preimages one replacement request may carry.
pub const MAX_REPLACEMENT_PREIMAGES: usize = 16;
/// Length of a SHA-256 hex digest.
pub const SHA256_HEX_LENGTH: usize = 64;

const CHAT_PATH: &str = "/v1/chat/completions";

/// Configured local inference failed or returned an invalid response.
#[derive(Debug, thiserror::Error)]
pub enum LocalInferenceError {
    /// Rule violation (Python `LocalInferenceError`).
    #[error("{0}")]
    Message(String),
    /// Rule violation raised `from` an underlying failure.
    #[error("{message}")]
    Caused {
        /// Python-compatible message.
        message: &'static str,
        /// The failure that triggered it.
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

impl LocalInferenceError {
    /// Build a rule-violation error.
    #[must_use]
    pub fn msg(message: &str) -> Self {
        Self::Message(message.to_owned())
    }

    /// Build a rule-violation error chained to `source`.
    #[must_use]
    pub fn caused(message: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self::Caused {
            message,
            source: Box::new(source),
        }
    }
}

fn fail<T>(message: &str) -> Result<T, LocalInferenceError> {
    Err(LocalInferenceError::msg(message))
}

/// Parts of a URL as Python `urllib.parse.urlsplit` reports them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SplitUrl {
    pub(crate) scheme: String,
    pub(crate) netloc: String,
    pub(crate) hostname: Option<String>,
    /// `Err` carries the message Python's `.port` raises as `ValueError`.
    pub(crate) port: Result<Option<u16>, String>,
    pub(crate) has_userinfo: bool,
    pub(crate) path: String,
    pub(crate) query: String,
    pub(crate) fragment: String,
}

fn valid_scheme(candidate: &str) -> bool {
    let mut chars = candidate.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn parse_port(port: &str) -> Result<Option<u16>, String> {
    if port.is_empty() {
        return Ok(None);
    }
    if !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "Port could not be cast to integer value as '{port}'"
        ));
    }
    port.parse::<u16>()
        .map(Some)
        .map_err(|_| "Port out of range 0-65535".to_owned())
}

fn valid_bracketed_host(host: &str) -> bool {
    if let Some(future) = host.strip_prefix('v') {
        return future.split_once('.').is_some_and(|(version, address)| {
            !version.is_empty()
                && version.chars().all(|ch| ch.is_ascii_hexdigit())
                && !address.is_empty()
                && !address.contains('\n')
        });
    }
    host.split_once('%')
        .map_or(host, |(address, _)| address)
        .parse::<Ipv6Addr>()
        .is_ok()
}

impl SplitUrl {
    /// `None` mirrors the `ValueError` for invalid bracketed netlocs.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let cleaned: String = raw
            .trim_start_matches(|c: char| c <= ' ')
            .chars()
            .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
            .collect();
        let (scheme, rest) = match cleaned.split_once(':') {
            Some((head, tail)) if valid_scheme(head) => (head.to_ascii_lowercase(), tail),
            _ => (String::new(), cleaned.as_str()),
        };
        let (netloc, rest) = rest.strip_prefix("//").map_or(("", rest), |after| {
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            after.split_at(end)
        });
        if netloc.contains('[') != netloc.contains(']') {
            return None;
        }
        let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (has_userinfo, hostinfo) = netloc
            .rsplit_once('@')
            .map_or((false, netloc), |(_, host)| (true, host));
        let (host, port) = if let Some((before, bracketed)) = hostinfo.split_once('[') {
            let (host, after) = bracketed.split_once(']')?;
            if !before.is_empty()
                || (!after.is_empty() && !after.starts_with(':'))
                || !valid_bracketed_host(host)
            {
                return None;
            }
            (host, after.split_once(':').map_or("", |(_, port)| port))
        } else {
            hostinfo.split_once(':').unwrap_or((hostinfo, ""))
        };
        Some(Self {
            scheme,
            netloc: netloc.to_owned(),
            hostname: (!host.is_empty()).then(|| host.to_lowercase()),
            port: parse_port(port),
            has_userinfo,
            path: path.to_owned(),
            query: query.to_owned(),
            fragment: fragment.to_owned(),
        })
    }

    /// Whether the hostname is one of the two loopback literals.
    pub(crate) fn is_loopback(&self) -> bool {
        matches!(self.hostname.as_deref(), Some("127.0.0.1" | "::1"))
    }
}

/// Reject URL credentials, query/fragment and remote or alternate endpoints.
fn allowed_chat_url(parsed: Option<&SplitUrl>) -> bool {
    parsed.is_some_and(|url| {
        url.scheme == "http"
            && url.is_loopback()
            && !url.has_userinfo
            && url.path == CHAT_PATH
            && url.query.is_empty()
            && url.fragment.is_empty()
    })
}

/// Allowlisted local HTTP endpoint, model and bounded inference budget.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalEndpoint {
    /// Loopback chat-completions URL.
    pub url: String,
    /// Model identifier.
    pub model: String,
    /// Request timeout in seconds.
    pub timeout_seconds: f64,
    /// Completion token budget.
    pub max_tokens: i64,
}

impl LocalEndpoint {
    /// Endpoint with the default 30 second timeout and 2048 token budget.
    #[must_use]
    pub fn new(url: &str, model: &str) -> Self {
        Self {
            url: url.to_owned(),
            model: model.to_owned(),
            timeout_seconds: 30.0,
            max_tokens: 2048,
        }
    }

    /// Replace the timeout.
    #[must_use]
    pub const fn with_timeout_seconds(mut self, seconds: f64) -> Self {
        self.timeout_seconds = seconds;
        self
    }

    /// Replace the token budget.
    #[must_use]
    pub const fn with_max_tokens(mut self, tokens: i64) -> Self {
        self.max_tokens = tokens;
        self
    }

    /// Check the URL allowlist, model identifier and inference budget.
    ///
    /// # Errors
    ///
    /// Returns [`LocalInferenceError`] for a non-loopback or non-canonical URL, an invalid model
    /// identifier, or an out-of-range timeout or token budget.
    pub fn validate(&self) -> Result<(), LocalInferenceError> {
        if !allowed_chat_url(SplitUrl::parse(&self.url).as_ref()) {
            return fail("Local inference requires an explicit loopback chat-completions URL");
        }
        if self.model.chars().all(is_py_space) || self.model.chars().count() > MAX_MODEL_ID_LENGTH {
            return fail("Invalid local model identifier");
        }
        let seconds_ok = (1.0..=f64::from(MAX_INFERENCE_SECONDS)).contains(&self.timeout_seconds);
        if !seconds_ok || !(1..=MAX_INFERENCE_TOKENS).contains(&self.max_tokens) {
            return fail("Invalid inference budget");
        }
        Ok(())
    }
}

/// Injectable transport for model-response qualification and offline tests.
pub trait Transport {
    /// Validate the endpoint under this transport's trust policy.
    /// # Errors
    /// Rejects endpoints outside the transport's approved scope or budget.
    fn validate(&self, endpoint: &LocalEndpoint) -> Result<(), LocalInferenceError> {
        endpoint.validate()
    }

    /// POST `payload` to the endpoint and return the raw response body.
    ///
    /// # Errors
    ///
    /// Returns [`LocalInferenceError`] when the request fails or the response is rejected.
    fn complete(
        &self,
        endpoint: &LocalEndpoint,
        payload: &[u8],
    ) -> Result<Vec<u8>, LocalInferenceError>;
}

/// Explicit loopback HTTP request; no proxy, redirect or credential handling.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopbackTransport;

impl Transport for LoopbackTransport {
    fn complete(
        &self,
        endpoint: &LocalEndpoint,
        payload: &[u8],
    ) -> Result<Vec<u8>, LocalInferenceError> {
        endpoint.validate()?;
        let request_failed = "Local inference request failed";
        let timeout = Duration::try_from_secs_f64(endpoint.timeout_seconds)
            .map_err(|error| LocalInferenceError::caused(request_failed, error))?;
        let response = UreqTransport
            .post_json(&endpoint.url, payload, timeout, MAX_RESPONSE_BYTES + 1)
            .map_err(|error| LocalInferenceError::caused(request_failed, error))?;
        if (300..400).contains(&response.status) {
            return fail(request_failed);
        }
        if response.status != HTTP_OK {
            return fail("Local endpoint returned unsuccessful HTTP status");
        }
        if response.body.len() > MAX_RESPONSE_BYTES {
            return fail("Local endpoint response exceeds limit");
        }
        Ok(response.body)
    }
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    stream: bool,
    max_tokens: i64,
    messages: [ChatMessage<'a>; 2],
}

/// Return bounded model text; output is NOT trusted tool instructions.
///
/// # Errors
///
/// Returns [`LocalInferenceError`] for an invalid endpoint, empty or oversized input, a transport
/// failure, or a response that is not exactly one non-empty bounded text choice.
pub fn complete_text(
    endpoint: &LocalEndpoint,
    system: &str,
    prompt: &str,
    transport: &dyn Transport,
) -> Result<String, LocalInferenceError> {
    transport.validate(endpoint)?;
    if system.is_empty()
        || prompt.is_empty()
        || system.chars().count() + prompt.chars().count() > MAX_PROMPT_CHARS
    {
        return fail("Invalid or oversized inference input");
    }
    let payload = ordered_json_spaced(&ChatRequest {
        model: &endpoint.model,
        stream: false,
        max_tokens: endpoint.max_tokens,
        messages: [
            ChatMessage {
                role: "system",
                content: system,
            },
            ChatMessage {
                role: "user",
                content: prompt,
            },
        ],
    })?;
    let raw = transport.complete(endpoint, payload.as_bytes())?;
    if raw.len() > MAX_RESPONSE_BYTES {
        return fail("Unbounded local model response");
    }
    let document: Value = serde_json::from_slice(&raw)
        .map_err(|error| LocalInferenceError::caused("Invalid local inference JSON", error))?;
    let Some(object) = document.as_object() else {
        return fail("Invalid local inference response");
    };
    let choices = object.get("choices").and_then(Value::as_array);
    let Some([choice]) = choices.map(Vec::as_slice) else {
        return fail("Local inference must return one choice");
    };
    let Some(choice) = choice.as_object() else {
        return fail("Invalid local inference choice");
    };
    let Some(text) = choice
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
    else {
        return fail("Missing local inference content");
    };
    if text.chars().all(is_py_space) || text.chars().count() > MAX_RESPONSE_BYTES {
        return fail("Empty or oversized local model text");
    }
    Ok(text.to_owned())
}

/// Ask a configured local model for a plan; validate before any dispatch.
///
/// # Errors
///
/// Returns [`LocalInferenceError`] when inference fails or the output fails plan admission.
pub fn propose_plan(
    endpoint: &LocalEndpoint,
    task: &str,
    transport: &dyn Transport,
) -> Result<Plan, LocalInferenceError> {
    let system = "Return ONLY one JSON plan with id, objective, primary_identity, nodes. \
Each node has id, objective, owned_paths, dependencies and role. \
Use coding by default. No markdown or prose.";
    let output = complete_text(endpoint, system, task, transport)?;
    parse_plan_json(&output).map_err(|error| {
        LocalInferenceError::caused("Local coordinator output failed plan admission", error)
    })
}

#[derive(Serialize)]
struct EditPrompt<'a> {
    task_id: &'a str,
    objective: &'a str,
    owned_paths: &'a [String],
}

/// Generate bounded proposed file content without applying or executing it.
///
/// # Errors
///
/// Returns [`LocalInferenceError`] when inference fails or the output fails ownership admission.
pub fn propose_edits(
    endpoint: &LocalEndpoint,
    task_id: &str,
    objective: &str,
    owned_paths: &[String],
    transport: &dyn Transport,
) -> Result<EditProposal, LocalInferenceError> {
    let system = "Return ONLY JSON with task_id and files. Each file has path and content. \
Do not use markdown, shell commands, or paths outside the provided ownership.";
    let prompt = ordered_json_spaced(&EditPrompt {
        task_id,
        objective,
        owned_paths,
    })?;
    let text = complete_text(endpoint, system, &prompt, transport)?;
    parse_edit_proposal(&text, task_id, owned_paths).map_err(|error| {
        LocalInferenceError::caused("Local coding output failed ownership admission", error)
    })
}

#[derive(Serialize)]
struct ReplacementPrompt<'a> {
    task_id: &'a str,
    objective: &'a str,
    owned_paths: &'a [String],
    preimages: &'a OrderedMap<String>,
}

fn check_preimages(
    owned_paths: &[String],
    preimages: &OrderedMap<String>,
) -> Result<(), LocalInferenceError> {
    if preimages.is_empty() || preimages.len() > MAX_REPLACEMENT_PREIMAGES {
        return fail("Expected bounded existing-file preimages");
    }
    for (name, digest) in preimages.iter() {
        let owned = is_owned(name, owned_paths);
        if !owned {
            return fail("Preimage path outside task ownership");
        }
        let hex_ok = digest.chars().all(|ch| matches!(ch, '0'..='9' | 'a'..='f'));
        if digest.chars().count() != SHA256_HEX_LENGTH || !hex_ok {
            return fail("Invalid preimage hash");
        }
    }
    Ok(())
}

/// Generate existing-file replacements; model must echo preimage hashes.
///
/// # Errors
///
/// Returns [`LocalInferenceError`] for unbounded, unowned or malformed preimages, a failed
/// inference, output that fails admission, or a replacement that does not match its preimage.
pub fn propose_replacements(
    endpoint: &LocalEndpoint,
    task_id: &str,
    objective: &str,
    owned_paths: &[String],
    preimages: &OrderedMap<String>,
    transport: &dyn Transport,
) -> Result<Vec<FileReplacement>, LocalInferenceError> {
    check_preimages(owned_paths, preimages)?;
    let system = "Return ONLY JSON with task_id and replacements; each replacement has \
path, expected_sha256 and content. Use only paths and hashes supplied.";
    let prompt = ordered_json_spaced(&ReplacementPrompt {
        task_id,
        objective,
        owned_paths,
        preimages,
    })?;
    let output = complete_text(endpoint, system, &prompt, transport)?;
    let replacements =
        parse_replacement_proposal(&output, task_id, owned_paths).map_err(|error| {
            LocalInferenceError::caused("Local replacement response failed admission", error)
        })?;
    if replacements
        .iter()
        .any(|item| preimages.get(&item.path) != Some(&item.expected_sha256))
    {
        return fail("Local replacement does not match supplied preimage");
    }
    Ok(replacements)
}

#[cfg(test)]
mod tests {
    use super::SplitUrl;

    #[test]
    fn bracketed_netloc_validation() {
        for host in [
            "[127.0.0.1]",
            "x[::1]",
            "[::1]x",
            "[zz]",
            "[::1",
            "::1]",
            "[v.foo]",
            "[vG.foo]",
            "[v1.]",
            "user@x[::1]",
        ] {
            assert!(
                SplitUrl::parse(&format!("http://{host}:11434/v1/chat/completions")).is_none(),
                "{host}"
            );
        }
        for host in ["[::1]", "[::1%eth0]", "[v1.future]", "user@[::1]"] {
            assert!(
                SplitUrl::parse(&format!("http://{host}:11434/v1/chat/completions")).is_some(),
                "{host}"
            );
        }
        let parsed = SplitUrl::parse("http://[::1%eth0]:11434/v1/chat/completions");
        assert!(
            parsed.as_ref().is_some_and(
                |url| url.hostname.as_deref() == Some("::1%eth0") && !url.is_loopback()
            )
        );
    }
}
