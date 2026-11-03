//! Single-lane tool-free Ollama requests. No Hermes gateway work is created.

use std::fs::{self, OpenOptions};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::core::routing::{Role, Route, RoutingError, select};

/// The only endpoint this adapter will contact.
pub const ENDPOINT: &str = "http://127.0.0.1:11434/api/generate";
/// Maximum accepted response body size in bytes.
pub const MAX_RESPONSE_BYTES: usize = 1_000_000;

/// Errors from the Ollama adapter.
#[derive(Debug, thiserror::Error)]
pub enum OllamaError {
    /// Route selection refused (Python `PermissionError`).
    #[error(transparent)]
    Route(#[from] RoutingError),
    /// Endpoint or backend refused (Python `PermissionError`).
    #[error("{0}")]
    Denied(String),
    /// Transport or status failure (Python `RuntimeError`).
    #[error("{0}")]
    Runtime(String),
    /// Transport failed with an underlying cause.
    #[error("Ollama request failed; verify idle state before retry")]
    Transport(#[source] TransportError),
    /// Malformed response shape (Python `TypeError`).
    #[error("{0}")]
    Type(String),
    /// Response over budget (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// Response was not valid JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Lock file failure.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Per-call options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaOptions {
    /// Endpoint URL; must equal [`ENDPOINT`].
    pub url: String,
    /// GPU lane lock file.
    pub lock_file: PathBuf,
}

impl Default for OllamaOptions {
    fn default() -> Self {
        Self {
            url: ENDPOINT.to_owned(),
            lock_file: PathBuf::from(".harness-runtime/gpu.lock"),
        }
    }
}

/// A completed HTTP exchange as seen by the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Body, read up to `MAX_RESPONSE_BYTES + 1` bytes.
    pub body: Vec<u8>,
}

/// Connection-level failure (Python `URLError` / `TimeoutError`).
#[derive(Debug)]
pub struct TransportError {
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl From<io::Error> for TransportError {
    fn from(source: io::Error) -> Self {
        Self {
            source: Box::new(source),
        }
    }
}

impl From<ureq::Error> for TransportError {
    fn from(source: ureq::Error) -> Self {
        Self {
            source: Box::new(source),
        }
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Ollama request failed; verify idle state before retry")
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Injectable HTTP transport so tests stay offline.
pub trait Transport {
    /// POST a JSON body and return the status plus at most `limit` body bytes.
    fn post_json(
        &self,
        url: &str,
        body: &[u8],
        timeout: Duration,
        limit: usize,
    ) -> Result<HttpResponse, TransportError>;
}

/// Real transport over `ureq`.
#[derive(Debug, Clone, Copy, Default)]
pub struct UreqTransport;

impl Transport for UreqTransport {
    fn post_json(
        &self,
        url: &str,
        body: &[u8],
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
        let response = agent
            .post(url)
            .header("Content-Type", "application/json")
            .send(body)
            .map_err(TransportError::from)?;
        let status = response.status().as_u16();
        let mut data = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(u64::try_from(limit).unwrap_or(u64::MAX))
            .read_to_end(&mut data)
            .map_err(TransportError::from)?;
        Ok(HttpResponse { status, body: data })
    }
}

/// Hold an exclusive advisory lock on `lock_file` while `work` runs.
///
/// # Errors
/// The lock directory or file cannot be opened, locked, canonicalized, or unlocked.
pub fn gpu_lane<T>(lock_file: &Path, work: impl FnOnce() -> T) -> io::Result<T> {
    if let Some(parent) = lock_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let handle = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(lock_file)?;
    #[cfg(not(unix))]
    let _guard = {
        let key = fs::canonicalize(lock_file)?;
        crate::util::process_lock::acquire(&key);
        crate::util::process_lock::Guard(key)
    };
    lock_exclusive(&handle)?;
    let result = work();
    unlock(&handle)?;
    Ok(result)
}

#[cfg(unix)]
fn lock_exclusive(handle: &fs::File) -> io::Result<()> {
    rustix::fs::flock(handle, rustix::fs::FlockOperation::LockExclusive).map_err(io::Error::from)
}

#[cfg(unix)]
fn unlock(handle: &fs::File) -> io::Result<()> {
    rustix::fs::flock(handle, rustix::fs::FlockOperation::Unlock).map_err(io::Error::from)
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // mirrors the unix signature
const fn lock_exclusive(_handle: &fs::File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // mirrors the unix signature
const fn unlock(_handle: &fs::File) -> io::Result<()> {
    Ok(())
}

fn decode(body: &[u8]) -> Result<String, OllamaError> {
    let data: Value = serde_json::from_slice(body)?;
    let Value::Object(map) = data else {
        return Err(OllamaError::Type(
            "Invalid Ollama response: expected JSON object".to_owned(),
        ));
    };
    match map.get("response") {
        Some(Value::String(text)) => Ok(text.clone()),
        _ => Err(OllamaError::Type(
            "Invalid Ollama response: expected string response".to_owned(),
        )),
    }
}

#[derive(Serialize)]
struct GenerateOptions {
    num_predict: u32,
    num_ctx: u32,
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    options: GenerateOptions,
    keep_alive: &'static str,
}

fn request_body(prompt: &str, route: &Route, url: &str) -> Result<Vec<u8>, OllamaError> {
    // Exact endpoint check prevents non-HTTP schemes, SSRF and accidental remote calls.
    if url != ENDPOINT {
        return Err(OllamaError::Denied(
            "Only the loopback Ollama endpoint is enabled in milestone 1".to_owned(),
        ));
    }
    let payload = GenerateRequest {
        model: &route.model,
        prompt,
        stream: false,
        options: GenerateOptions {
            num_predict: route.max_output_tokens,
            num_ctx: 8192,
        },
        keep_alive: "0s",
    };
    Ok(serde_json::to_vec(&payload)?)
}

fn read_response(response: HttpResponse) -> Result<Vec<u8>, OllamaError> {
    if response.status != 200 {
        return Err(OllamaError::Runtime(format!(
            "Ollama HTTP status {}",
            response.status
        )));
    }
    Ok(response.body)
}

fn fetch(
    transport: &dyn Transport,
    url: &str,
    body: &[u8],
    timeout_seconds: u32,
) -> Result<Vec<u8>, OllamaError> {
    let timeout = Duration::from_secs(u64::from(timeout_seconds));
    let response = transport
        .post_json(url, body, timeout, MAX_RESPONSE_BYTES + 1)
        .map_err(OllamaError::Transport)?;
    let data = read_response(response)?;
    if data.len() > MAX_RESPONSE_BYTES {
        return Err(OllamaError::Value(
            "Ollama response exceeds byte budget".to_owned(),
        ));
    }
    Ok(data)
}

/// Run a prompt through `transport` on the single GPU lane.
///
/// # Errors
/// Route or endpoint validation, lane locking, transport, or response decoding fails.
pub fn text_task_with(
    transport: &dyn Transport,
    prompt: &str,
    role: Role,
    options: Option<&OllamaOptions>,
) -> Result<String, OllamaError> {
    let route = select(role, false)?;
    if route.backend != "ollama" || route.tool_access {
        return Err(OllamaError::Denied(
            "Only tool-free local routes supported by this adapter".to_owned(),
        ));
    }
    let default = OllamaOptions::default();
    let opts = options.unwrap_or(&default);
    let body = request_body(prompt, &route, &opts.url)?;
    let data = gpu_lane(&opts.lock_file, || {
        fetch(transport, &opts.url, &body, route.max_seconds)
    })??;
    decode(&data)
}

/// Run a prompt against the real loopback Ollama endpoint.
///
/// # Errors
/// Route or endpoint validation, lane locking, transport, or response decoding fails.
pub fn text_task(
    prompt: &str,
    role: Role,
    options: Option<&OllamaOptions>,
) -> Result<String, OllamaError> {
    text_task_with(&UreqTransport, prompt, role, options)
}
