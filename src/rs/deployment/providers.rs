//! Explicit operator registry and credential-safe inference transport.
use crate::util::PathLexical as _;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::multiagent::local_inference::{
    LocalEndpoint, LocalInferenceError, MAX_RESPONSE_BYTES, SplitUrl, Transport,
};
use crate::multiagent::ollama_native::{NativeOllamaJsonTransport, OrderedJson, Schema};
use crate::util::ordered_map::OrderedMap;
use crate::util::{JsonError, ordered_json_spaced};

/// Maximum registry size.
pub const MAX_REGISTRY_BYTES: u64 = 32_768;
/// Minimum Hermes role context.
pub const MIN_HERMES_CONTEXT: i64 = 65_536;
/// Maximum declared context.
pub const MAX_CONTEXT: i64 = 262_144;
/// Maximum request timeout.
pub const MAX_PROVIDER_TIMEOUT_SECONDS: u32 = 120;
/// Maximum output budget.
pub const MAX_PROVIDER_OUTPUT_TOKENS: i64 = 8192;
/// Categorized provider failure; never contains credentials.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// Safe category.
    #[error("{0}")]
    Message(String),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
    /// Native local inference failure.
    #[error(transparent)]
    Inference(#[from] LocalInferenceError),
}
fn fail<T>(message: &str) -> Result<T, ProviderError> {
    Err(ProviderError::Message(message.to_owned()))
}
const fn one() -> i64 {
    1
}
/// Operator-controlled placement, never model-declared authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSpec {
    /// Profile name.
    pub name: String,
    /// Backend identity.
    pub backend: String,
    /// Model identity.
    pub model: String,
    /// Approved URL.
    pub url: String,
    /// Declared context.
    pub context_tokens: i64,
    /// Credential environment reference, never a secret.
    #[serde(default)]
    pub key_env: String,
    /// Declared tool support.
    #[serde(default)]
    pub tool_calls: bool,
    /// Approved service.
    #[serde(default)]
    pub unit: String,
    /// Owning GPU host.
    #[serde(default)]
    pub gpu_host: String,
    /// Only one concurrent call is allowed.
    #[serde(default = "one")]
    pub max_concurrency: i64,
}
impl ProviderSpec {
    /// Validate identity, URL, budget, and credential reference.
    /// # Errors
    /// Returns a safe category for invalid operator profiles.
    pub fn validate(&self) -> Result<(), ProviderError> {
        let valid_model = self.model.len() <= 128
            && self
                .model
                .bytes()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
            && self
                .model
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:/-".contains(&c))
            && self
                .model
                .split('/')
                .all(|part| !matches!(part, "" | "." | ".."));
        if self.name.is_empty()
            || !valid_model
            || !matches!(self.backend.as_str(), "ollama" | "llama.cpp")
        {
            return fail("invalid-provider-identity");
        }
        if !(1..=MAX_CONTEXT).contains(&self.context_tokens) || self.max_concurrency != 1 {
            return fail("invalid-context-or-concurrency");
        }
        self.validate_url()?;
        if !self.key_env.is_empty()
            && (!self.key_env.chars().next().is_some_and(char::is_alphabetic)
                || !self
                    .key_env
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_'))
        {
            return fail("invalid-credential-reference");
        }
        if self.backend == "llama.cpp" && self.key_env.is_empty() {
            return fail("remote-bearer-auth-required");
        }
        if self.backend == "ollama" && !self.key_env.is_empty() {
            return fail("ollama-native-adapter-does-not-support-auth");
        }
        if !self.unit.is_empty() && (!self.unit.ends_with(".service") || self.unit.contains('/')) {
            return fail("invalid-systemd-unit");
        }
        Ok(())
    }
    fn validate_url(&self) -> Result<(), ProviderError> {
        let parts = SplitUrl::parse(&self.url)
            .ok_or_else(|| ProviderError::Message("unsafe-endpoint-url".to_owned()))?;
        if parts.has_userinfo || !parts.query.is_empty() || !parts.fragment.is_empty() {
            return fail("unsafe-endpoint-url");
        }
        if parts.path != "/v1/chat/completions" {
            return fail("invalid-endpoint-path");
        }
        if self.backend == "ollama" {
            if parts.scheme != "http" || !parts.is_loopback() {
                return fail("ollama-must-be-loopback");
            }
        } else if parts.scheme != "https"
            || !parts
                .hostname
                .as_deref()
                .is_some_and(|host| host.ends_with(".home.arpa"))
        {
            return fail("remote-endpoint-must-use-trusted-internal-https");
        }
        Ok(())
    }
    /// Admit an explicitly configured role.
    /// # Errors
    /// Hermes roles always require independent native receipt qualification.
    pub fn admit_role(&self, role: &str) -> Result<(), ProviderError> {
        self.validate()?;
        if matches!(role, "hermes-tool-worker" | "hermes-coordinator") {
            if self.context_tokens < MIN_HERMES_CONTEXT {
                return fail("insufficient-context-for-hermes-role");
            }
            if !self.tool_calls {
                return fail("tool-calling-not-qualified");
            }
            return fail("Hermes-native-tool-receipt-required-for-admission");
        }
        if role == "security-local" && !matches!(self.backend.as_str(), "ollama" | "llama.cpp") {
            return fail("nonlocal-security-role");
        }
        Ok(())
    }
}
/// Credential lookup seam for offline tests without process environment mutation.
pub trait Credentials {
    /// Resolve an environment reference.
    fn lookup(&self, name: &str) -> Option<String>;
}
impl<F: Fn(&str) -> Option<String>> Credentials for F {
    fn lookup(&self, name: &str) -> Option<String> {
        self(name)
    }
}
/// Real environment lookup.
#[derive(Debug, Clone, Copy, Default)]
pub struct Environment;
impl Credentials for Environment {
    fn lookup(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}
/// Exactly one approved endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredEndpoint {
    /// Common inference endpoint.
    pub endpoint: LocalEndpoint,
    /// Registry identity.
    pub spec: Option<ProviderSpec>,
}
impl ConfiguredEndpoint {
    /// Validate using environment credentials.
    /// # Errors
    /// Rejects mismatches, invalid budgets, or missing credentials.
    pub fn validate(&self) -> Result<(), ProviderError> {
        self.validate_with(&Environment)
    }
    /// Validate the endpoint with caller-supplied credentials.
    /// # Errors
    /// Rejects mismatches, invalid budgets, or missing credentials.
    pub fn validate_with(&self, credentials: &dyn Credentials) -> Result<(), ProviderError> {
        let Some(spec) = &self.spec else {
            return fail("endpoint-does-not-match-registry");
        };
        if spec.url != self.endpoint.url || spec.model != self.endpoint.model {
            return fail("endpoint-does-not-match-registry");
        }
        spec.validate()?;
        if !(1.0..=f64::from(MAX_PROVIDER_TIMEOUT_SECONDS)).contains(&self.endpoint.timeout_seconds)
            || !(1..=MAX_PROVIDER_OUTPUT_TOKENS).contains(&self.endpoint.max_tokens)
        {
            return fail("invalid-inference-budget");
        }
        if !spec.key_env.is_empty()
            && credentials
                .lookup(&spec.key_env)
                .is_none_or(|key| key.is_empty())
        {
            return fail("credential-environment-missing");
        }
        Ok(())
    }
}
/// Borrowed HTTP request with credential-safe debugging.
pub struct HttpRequest<'a> {
    /// Exact URL.
    pub url: &'a str,
    /// GET or POST.
    pub method: &'a str,
    /// Headers, including sensitive credentials.
    pub headers: &'a [(String, String)],
    /// Request body.
    pub payload: &'a [u8],
    /// Timeout.
    pub timeout: Duration,
}
impl std::fmt::Debug for HttpRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}
/// Narrow HTTP seam returning safe, categorized failures.
pub trait Opener {
    /// Execute a request and return its bounded body.
    /// # Errors
    /// Returns categorized HTTP, timeout, TLS, or size failures.
    fn open(&self, request: &HttpRequest<'_>) -> Result<Vec<u8>, ProviderError>;
}
/// TLS-verifying HTTP without redirects or proxies.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecureOpener;
impl Opener for SecureOpener {
    fn open(&self, request: &HttpRequest<'_>) -> Result<Vec<u8>, ProviderError> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(request.timeout))
            .build()
            .into();
        let mut req = ureq::http::Request::builder()
            .method(request.method)
            .uri(request.url);
        for (name, value) in request.headers {
            req = req.header(name.as_str(), value);
        }
        let reply = req
            .body(if request.method == "GET" {
                &[][..]
            } else {
                request.payload
            })
            .map_err(ureq::Error::from)
            .and_then(|req| agent.run(req));
        let response = match reply {
            Ok(response) if response.status() == 200 => response,
            Ok(_) => return fail("provider-http-failure"),
            Err(ureq::Error::StatusCode(status)) => {
                return fail(match status {
                    401 | 403 => "provider-authentication-rejected",
                    429 => "provider-rate-limited",
                    500.. => "provider-server-failure",
                    _ => "provider-http-failure",
                });
            }
            Err(error) => {
                let timeout = matches!(error, ureq::Error::Timeout(_))
                    || matches!(&error, ureq::Error::Io(error) if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ));
                return fail(if timeout {
                    "provider-timeout"
                } else {
                    "provider-unreachable-or-tls-failure"
                });
            }
        };
        let mut content = Vec::new();
        response
            .into_body()
            .into_reader()
            .take((MAX_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut content)
            .map_err(|error| {
                ProviderError::Message(
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) {
                        "provider-timeout"
                    } else {
                        "provider-unreachable-or-tls-failure"
                    }
                    .to_owned(),
                )
            })?;
        bounded(content)
    }
}
fn bounded(content: Vec<u8>) -> Result<Vec<u8>, ProviderError> {
    if content.len() > MAX_RESPONSE_BYTES {
        return fail("provider-response-too-large");
    }
    Ok(content)
}
/// HTTPS llama.cpp adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAICompatibleTransport;
impl OpenAICompatibleTransport {
    /// Complete using the real credential-safe HTTP adapter.
    /// # Errors
    /// Returns categorized authentication, transport, or response failures.
    pub fn complete(
        &self,
        endpoint: &ConfiguredEndpoint,
        payload: &[u8],
    ) -> Result<Vec<u8>, ProviderError> {
        self.complete_with(endpoint, payload, &SecureOpener, &Environment)
    }
    /// Complete a request with caller-supplied HTTP transport and credentials.
    /// # Errors
    /// Rejects unregistered endpoints, transport failures, or model/schema mismatch.
    pub fn complete_with(
        &self,
        endpoint: &ConfiguredEndpoint,
        payload: &[u8],
        opener: &dyn Opener,
        credentials: &dyn Credentials,
    ) -> Result<Vec<u8>, ProviderError> {
        let Some(spec) = &endpoint.spec else {
            return fail("unregistered-openai-endpoint");
        };
        endpoint.validate_with(credentials)?;
        if spec.backend != "llama.cpp" {
            return fail("incorrect-provider-backend");
        }
        let key = credentials
            .lookup(&spec.key_env)
            .ok_or_else(|| ProviderError::Message("credential-environment-missing".to_owned()))?;
        let headers = vec![
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("Authorization".to_owned(), format!("Bearer {key}")),
        ];
        let raw = bounded(opener.open(&HttpRequest {
            url: &endpoint.endpoint.url,
            method: "POST",
            headers: &headers,
            payload,
            timeout: Duration::from_secs_f64(endpoint.endpoint.timeout_seconds),
        })?)?;
        let value: Value = serde_json::from_slice(&raw)
            .map_err(|_| ProviderError::Message("malformed-provider-completion".to_owned()))?;
        if !value.is_object()
            || value.get("model").and_then(Value::as_str) != Some(&endpoint.endpoint.model)
            || value
                .get("choices")
                .and_then(Value::as_array)
                .is_none_or(|v| v.len() != 1)
        {
            return fail("completion-model-identity-or-schema-mismatch");
        }
        Ok(raw)
    }
}
/// Role-to-profile registry without implicit fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRegistry {
    /// Profiles in operator order.
    pub profiles: OrderedMap<ProviderSpec>,
    /// Role bindings in operator order.
    pub roles: OrderedMap<String>,
}
impl ProviderRegistry {
    /// Resolve an explicitly configured role.
    /// # Errors
    /// Rejects unconfigured or inadmissible roles.
    pub fn role(&self, role: &str) -> Result<&ProviderSpec, ProviderError> {
        let spec = self
            .roles
            .get(role)
            .and_then(|name| self.profiles.get(name))
            .ok_or_else(|| ProviderError::Message("role-not-configured".to_owned()))?;
        spec.admit_role(role)?;
        Ok(spec)
    }
    /// Build a bounded registered endpoint.
    /// # Errors
    /// Rejects unconfigured or inadmissible roles.
    pub fn endpoint(&self, role: &str) -> Result<ConfiguredEndpoint, ProviderError> {
        let spec = self.role(role)?;
        Ok(ConfiguredEndpoint {
            endpoint: LocalEndpoint::new(&spec.url, &spec.model).with_timeout_seconds(120.0),
            spec: Some(spec.clone()),
        })
    }
    /// Route approved models through the existing local inference protocol.
    /// # Errors
    /// Rejects duplicate model routes, missing schemas, or invalid roles.
    pub fn transport(
        &self,
        roles: &[&str],
        schemas: &BTreeMap<String, Schema>,
    ) -> Result<RoutingTransport, ProviderError> {
        let mut routes = BTreeMap::new();
        let mut native = NativeOllamaJsonTransport::new();
        for role in roles {
            let spec = self.role(role)?;
            if routes.contains_key(&spec.model) {
                return fail("duplicate-model-id-in-routes");
            }
            if spec.backend == "ollama" {
                let schema = schemas
                    .get(*role)
                    .ok_or_else(|| ProviderError::Message((*role).to_owned()))?;
                native = native.with_schema(&spec.model, schema.clone());
            }
            routes.insert(spec.model.clone(), spec.clone());
        }
        Ok(RoutingTransport { routes, native })
    }
}
/// Approved routes implementing the shared inference transport.
#[derive(Debug, Clone)]
pub struct RoutingTransport {
    routes: BTreeMap<String, ProviderSpec>,
    native: NativeOllamaJsonTransport,
}
impl Transport for RoutingTransport {
    fn validate(&self, endpoint: &LocalEndpoint) -> Result<(), LocalInferenceError> {
        let spec = self
            .routes
            .get(&endpoint.model)
            .ok_or_else(|| LocalInferenceError::msg("model-not-in-approved-route"))?;
        ConfiguredEndpoint {
            endpoint: endpoint.clone(),
            spec: Some(spec.clone()),
        }
        .validate()
        .map_err(|e| LocalInferenceError::msg(&e.to_string()))
    }

    fn complete(
        &self,
        endpoint: &LocalEndpoint,
        payload: &[u8],
    ) -> Result<Vec<u8>, LocalInferenceError> {
        let spec = self
            .routes
            .get(&endpoint.model)
            .ok_or_else(|| LocalInferenceError::msg("model-not-in-approved-route"))?;
        if spec.backend == "ollama" {
            ConfiguredEndpoint {
                endpoint: endpoint.clone(),
                spec: Some(spec.clone()),
            }
            .validate()
            .map_err(|e| LocalInferenceError::msg(&e.to_string()))?;
            return self.native.complete(endpoint, payload);
        }
        OpenAICompatibleTransport
            .complete(
                &ConfiguredEndpoint {
                    endpoint: endpoint.clone(),
                    spec: Some(spec.clone()),
                },
                payload,
            )
            .map_err(|e| LocalInferenceError::msg(&e.to_string()))
    }
}
/// Read operator-authored profiles; reject symlinks and inline secrets.
/// # Errors
/// Rejects invalid files, profiles, and role mappings.
pub fn load_registry(path: &Path) -> Result<ProviderRegistry, ProviderError> {
    #[derive(Deserialize)]
    struct Roles {
        roles: OrderedMap<String>,
    }

    if path.is_symlink_lexical()
        || !path.is_file()
        || path
            .metadata()
            .map_or(true, |m| m.len() > MAX_REGISTRY_BYTES)
    {
        return fail("invalid-provider-registry-file");
    }
    let bytes = std::fs::read(path)
        .map_err(|_| ProviderError::Message("invalid-provider-registry-json".to_owned()))?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|_| ProviderError::Message("invalid-provider-registry-json".to_owned()))?;
    let document = document
        .as_object()
        .ok_or_else(|| ProviderError::Message("invalid-provider-registry-schema".to_owned()))?;
    if document.len() != 2 || !document.contains_key("profiles") || !document.contains_key("roles")
    {
        return fail("invalid-provider-registry-schema");
    }
    let raw = document
        .get("profiles")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Message("invalid-provider-registry-schema".to_owned()))?;
    if !document.get("roles").is_some_and(Value::is_object) {
        return fail("invalid-provider-registry-schema");
    }
    let fields = [
        "name",
        "backend",
        "model",
        "url",
        "context_tokens",
        "key_env",
        "tool_calls",
        "unit",
        "gpu_host",
        "max_concurrency",
    ];
    let mut profiles = OrderedMap::new();
    for value in raw {
        if !value
            .as_object()
            .is_some_and(|v| v.keys().all(|k| fields.contains(&k.as_str())))
        {
            return fail("unsupported-provider-registry-field");
        }
        let spec: ProviderSpec = serde_json::from_value(value.clone())
            .map_err(|_| ProviderError::Message("invalid-provider-registry-profile".to_owned()))?;
        spec.validate()?;
        if profiles.contains_key(&spec.name) {
            return fail("duplicate-provider-profile");
        }
        profiles.insert(spec.name.clone(), spec);
    }
    let roles: Roles = serde_json::from_slice(&bytes)
        .map_err(|_| ProviderError::Message("invalid-role-mapping".to_owned()))?;
    Ok(ProviderRegistry {
        profiles,
        roles: roles.roles,
    })
}
/// Live probe report in Python field order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // mirrors the Python report
pub struct ProviderProbe {
    /// Profile name.
    pub profile: String,
    /// Backend.
    pub backend: String,
    /// Model.
    pub model: String,
    /// Nonempty health response.
    pub health_response_received: bool,
    /// Model matched catalogue and completion.
    pub model_identity_verified: bool,
    /// Bearer-authenticated completion.
    pub authenticated_completion: bool,
    /// Operator declaration only.
    pub declared_context_tokens: i64,
    /// Requires an independent long prompt test.
    pub long_context_qualified: bool,
    /// Requires native terminal receipts.
    pub tool_calls_qualified: bool,
    /// Completion included usage.
    pub usage_available: bool,
}
/// Run bounded authenticated identity probes.
/// # Errors
/// Rejects invalid profiles, failed HTTP, or mismatched responses.
pub fn probe_provider(spec: &ProviderSpec) -> Result<ProviderProbe, ProviderError> {
    probe_provider_with(
        spec,
        &SecureOpener,
        &Environment,
        &NativeOllamaJsonTransport::new().with_schema(&spec.model, permissive_schema()),
    )
}
fn permissive_schema() -> Schema {
    [
        ("type".to_owned(), OrderedJson::from("object")),
        (
            "properties".to_owned(),
            OrderedJson::Object(OrderedMap::new()),
        ),
        ("additionalProperties".to_owned(), OrderedJson::Bool(true)),
    ]
    .into_iter()
    .collect()
}
/// Probe a provider with caller-supplied HTTP, credentials and native transport.
/// # Errors
/// Rejects failed probes and model/schema mismatches.
pub fn probe_provider_with(
    spec: &ProviderSpec,
    opener: &dyn Opener,
    credentials: &dyn Credentials,
    native: &dyn Transport,
) -> Result<ProviderProbe, ProviderError> {
    #[derive(Serialize)]
    struct Payload<'a> {
        model: &'a str,
        stream: bool,
        max_tokens: u8,
        messages: [Message; 1],
    }
    #[derive(Serialize)]
    struct Message {
        role: &'static str,
        content: &'static str,
    }

    spec.validate()?;
    let endpoint = ConfiguredEndpoint {
        endpoint: LocalEndpoint::new(&spec.url, &spec.model)
            .with_timeout_seconds(15.0)
            .with_max_tokens(32),
        spec: Some(spec.clone()),
    };
    endpoint.validate_with(credentials)?;
    let health = probe_catalogue(spec, opener, credentials)?;
    let ollama = spec.backend == "ollama";
    let payload = ordered_json_spaced(&Payload {
        model: &spec.model,
        stream: false,
        max_tokens: 8,
        messages: [Message {
            role: "user",
            content: "Reply OK.",
        }],
    })?;
    let completion = if ollama {
        native.complete(&endpoint.endpoint, payload.as_bytes())?
    } else {
        OpenAICompatibleTransport.complete_with(
            &endpoint,
            payload.as_bytes(),
            opener,
            credentials,
        )?
    };
    let document: Value = serde_json::from_slice(&completion)
        .map_err(|_| ProviderError::Message("malformed-provider-completion".to_owned()))?;
    let choices = document.get("choices").and_then(Value::as_array);
    if !choices.is_some_and(|c| {
        c.len() == 1
            && c.first()
                .and_then(|v| v.get("message"))
                .and_then(|v| v.get("content"))
                .is_some_and(Value::is_string)
    }) {
        return fail("malformed-provider-completion");
    }
    if !ollama && document.get("model").and_then(Value::as_str) != Some(&spec.model) {
        return fail("completion-model-identity-mismatch");
    }
    Ok(ProviderProbe {
        profile: spec.name.clone(),
        backend: spec.backend.clone(),
        model: spec.model.clone(),
        health_response_received: !health.is_empty(),
        model_identity_verified: true,
        authenticated_completion: !spec.key_env.is_empty(),
        declared_context_tokens: spec.context_tokens,
        long_context_qualified: false,
        tool_calls_qualified: false,
        usage_available: document.get("usage").is_some_and(Value::is_object),
    })
}

fn probe_catalogue(
    spec: &ProviderSpec,
    opener: &dyn Opener,
    credentials: &dyn Credentials,
) -> Result<Vec<u8>, ProviderError> {
    let ollama = spec.backend == "ollama";
    let parts = SplitUrl::parse(&spec.url)
        .ok_or_else(|| ProviderError::Message("unsafe-endpoint-url".to_owned()))?;
    let root = format!("{}://{}", parts.scheme, parts.netloc);
    let health_url = format!("{root}{}", if ollama { "/api/version" } else { "/health" });
    let models_url = format!("{root}{}", if ollama { "/api/tags" } else { "/v1/models" });
    let mut headers = vec![("Accept".to_owned(), "application/json".to_owned())];
    if !spec.key_env.is_empty() {
        headers.push((
            "Authorization".to_owned(),
            format!(
                "Bearer {}",
                credentials.lookup(&spec.key_env).unwrap_or_default()
            ),
        ));
    }
    let get = |url: &str| {
        bounded(opener.open(&HttpRequest {
            url,
            method: "GET",
            headers: &headers,
            payload: &[],
            timeout: Duration::from_secs(15),
        })?)
    };
    let health = get(&health_url)?;
    let models = get(&models_url)?;
    let catalogue: Value = serde_json::from_slice(&models)
        .map_err(|_| ProviderError::Message("malformed-model-catalogue".to_owned()))?;
    let entries = catalogue
        .get(if ollama { "models" } else { "data" })
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Message("malformed-model-catalogue".to_owned()))?;
    let key = if ollama { "name" } else { "id" };
    let mut found = false;
    for item in entries.iter().filter(|v| v.is_object()) {
        let id = item
            .get(key)
            .ok_or_else(|| ProviderError::Message("malformed-model-catalogue".to_owned()))?;
        found |= id.as_str() == Some(&spec.model);
    }
    if !found {
        return fail("remote-model-identity-mismatch");
    }
    Ok(health)
}
