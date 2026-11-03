//! Attach to an existing Hermes gateway, waiting briefly for it to become ready.

use crate::util::PyStr as _;
use std::thread::sleep;
use std::time::{Duration, Instant};

pub use super::gateway_readonly::{GatewayRpc, GatewayRpcError};

/// Default seconds to wait for the gateway to become ready.
pub const DEFAULT_READINESS_TIMEOUT: f64 = 15.0;
/// Default seconds between discovery polls.
pub const DEFAULT_READINESS_POLL_INTERVAL: f64 = 0.5;
/// Discovery states that justify polling again.
pub const TRANSIENT_STATES: [&str; 2] = ["absent", "starting"];

/// Errors raised while attaching to an existing gateway.
#[derive(Debug, thiserror::Error)]
pub enum GatewayAttachmentError {
    /// The existing gateway could not be safely identified or attached.
    #[error("{0}")]
    Attachment(String),
    /// Invalid configuration (Python `ValueError`).
    #[error("{0}")]
    InvalidConfig(String),
    /// The connector failed.
    #[error(transparent)]
    Rpc(#[from] GatewayRpcError),
}

/// Minimum result required to make an attach-only decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryResult<E> {
    /// Discovered endpoint, if any.
    pub endpoint: Option<E>,
    /// Discovery state label, if any.
    pub state: Option<String>,
}

/// Profile discovery; `Err` models any exception raised by discovery.
pub type DiscoverGateway<'a, E> = Box<dyn Fn(&str) -> Result<DiscoveryResult<E>, String> + 'a>;
/// Decides whether a discovery result is attachable.
pub type IsAttachable<'a, E> = Box<dyn Fn(&DiscoveryResult<E>) -> bool + 'a>;
/// Injected no-autostart connector.
pub type Connector<'a, E, R> = Box<dyn Fn(E) -> Result<R, GatewayRpcError> + 'a>;

/// Explicit profile discovery and injected no-autostart connector.
pub struct GatewayAttachmentConfig<'a, E, R> {
    /// Profile home path.
    pub profile_home: String,
    /// Discovery function.
    pub discover: DiscoverGateway<'a, E>,
    /// Attachability predicate.
    pub is_attachable: IsAttachable<'a, E>,
    /// Connector for the single ready endpoint.
    pub connector: Connector<'a, E, R>,
    /// Seconds to wait for readiness.
    pub readiness_timeout: f64,
    /// Seconds between polls.
    pub poll_interval: f64,
}

impl<'a, E, R> GatewayAttachmentConfig<'a, E, R> {
    /// Build a config with the default readiness timeout and poll interval.
    #[must_use]
    pub fn new(
        profile_home: &str,
        discover: DiscoverGateway<'a, E>,
        is_attachable: IsAttachable<'a, E>,
        connector: Connector<'a, E, R>,
    ) -> Self {
        Self {
            profile_home: profile_home.to_owned(),
            discover,
            is_attachable,
            connector,
            readiness_timeout: DEFAULT_READINESS_TIMEOUT,
            poll_interval: DEFAULT_READINESS_POLL_INTERVAL,
        }
    }
}

fn attachment(msg: &str) -> GatewayAttachmentError {
    GatewayAttachmentError::Attachment(msg.to_owned())
}

/// Validate the config before any discovery happens.
///
/// # Errors
/// The readiness timeout or polling interval is outside the permitted bounds.
pub fn validate_config<E, R>(
    config: &GatewayAttachmentConfig<'_, E, R>,
) -> Result<(), GatewayAttachmentError> {
    let invalid = |msg: &str| Err(GatewayAttachmentError::InvalidConfig(msg.to_owned()));
    if config.profile_home.py_strip().is_empty() {
        return invalid("profile_home must be a nonblank path");
    }
    if !(0.0..f64::INFINITY).contains(&config.readiness_timeout) {
        return invalid("readiness_timeout must be finite and nonnegative");
    }
    if !(config.poll_interval > 0.0 && config.poll_interval < f64::INFINITY) {
        return invalid("poll_interval must be finite and positive");
    }
    Ok(())
}

/// Poll discovery until a single attachable endpoint is ready.
///
/// # Errors
/// Configuration is invalid, discovery or readiness fails, or the connector fails.
pub fn ready_endpoint<E, R>(
    config: &GatewayAttachmentConfig<'_, E, R>,
) -> Result<E, GatewayAttachmentError> {
    // Python accepts any finite timeout; clamp huge values instead of panicking.
    let started = Instant::now();
    let deadline = Duration::try_from_secs_f64(config.readiness_timeout)
        .ok()
        .and_then(|wait| started.checked_add(wait))
        .or_else(|| started.checked_add(Duration::from_secs(100 * 365 * 86_400)))
        .unwrap_or(started);
    loop {
        let mut discovery = (config.discover)(&config.profile_home)
            .map_err(|_| attachment("Existing gateway discovery failed; no start attempted"))?;
        if discovery.endpoint.is_some()
            && (config.is_attachable)(&discovery)
            && let Some(endpoint) = discovery.endpoint.take()
        {
            return Ok(endpoint);
        }
        let transient = discovery
            .state
            .as_deref()
            .is_some_and(|state| TRANSIENT_STATES.contains(&state));
        if !transient {
            return Err(attachment(
                "No attachable existing gateway; no start attempted",
            ));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(attachment(
                "Existing gateway readiness timed out; no start attempted",
            ));
        }
        sleep(
            Duration::try_from_secs_f64(config.poll_interval)
                .unwrap_or(remaining)
                .min(remaining),
        );
    }
}

/// Wait for an *existing* gateway; never start, reconnect, or resubmit.
///
/// Discovery retries are limited to transient `absent` / `starting` states.
/// Only the single discovered ready endpoint is given to the injected connector.
/// `body` runs while the connection is open; the connection drops afterwards.
///
/// # Errors
/// Configuration is invalid, discovery or readiness fails, or the connector fails.
pub fn attach_existing_gateway<E, R, T>(
    config: &GatewayAttachmentConfig<'_, E, R>,
    body: impl FnOnce(&mut R) -> T,
) -> Result<T, GatewayAttachmentError>
where
    R: GatewayRpc,
{
    validate_config(config)?;
    let endpoint = ready_endpoint(config)?;
    let mut rpc = (config.connector)(endpoint)?;
    Ok(body(&mut rpc))
}

impl<E, R> std::fmt::Debug for GatewayAttachmentConfig<'_, E, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayAttachmentConfig")
            .field("profile_home", &self.profile_home)
            .field("readiness_timeout", &self.readiness_timeout)
            .field("poll_interval", &self.poll_interval)
            .finish_non_exhaustive()
    }
}
