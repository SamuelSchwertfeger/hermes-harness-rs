//! Enumerate authorized session identities through an existing Hermes gateway.
//!
//! No session is selected, resumed, created, or modified. Only session.list is sent.

use crate::util::PyStr as _;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::gateway_attachment::{
    GatewayAttachmentConfig, GatewayAttachmentError, attach_existing_gateway,
};
use super::gateway_readonly::{GatewayReadError, GatewayRpc, GatewayRpcError};

/// Largest session sample a caller may request.
pub const MAX_SESSION_SAMPLE: usize = 20;

/// Errors raised by session discovery.
#[derive(Debug, thiserror::Error)]
pub enum SessionDiscoveryError {
    /// The listing violated the read-only contract.
    #[error(transparent)]
    Read(#[from] GatewayReadError),
    /// The existing gateway could not be attached.
    #[error(transparent)]
    Attachment(#[from] GatewayAttachmentError),
    /// RPC failure propagated without retry.
    #[error(transparent)]
    Rpc(#[from] GatewayRpcError),
    /// Invalid argument (Python `ValueError`).
    #[error("{0}")]
    Value(String),
}

/// Listing scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionScope {
    /// Currently live sessions.
    Live,
    /// Stored sessions.
    Stored,
}

impl SessionScope {
    /// The Python literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Stored => "stored",
        }
    }
}

/// Only the identifier and running status, never transcript metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCandidate {
    /// Session identity.
    pub session_id: String,
    /// Whether the session is running.
    pub running: bool,
}

/// Bounded view of sessions already accessible to the authenticated actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDiscoveryReport {
    /// Listing scope.
    pub scope: SessionScope,
    /// Parsed candidates.
    pub sessions: Vec<SessionCandidate>,
    /// Requested limit.
    pub limit: usize,
}

fn read_error(msg: &str) -> GatewayReadError {
    GatewayReadError::Read(msg.to_owned())
}

fn parse_row(
    row: &Value,
    seen: &mut BTreeSet<String>,
) -> Result<SessionCandidate, GatewayReadError> {
    let Some(object) = row.as_object() else {
        return Err(read_error(
            "Gateway session listing contains a malformed row",
        ));
    };
    let session_id = object
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| !id.py_strip().is_empty() && !seen.contains(*id))
        .ok_or_else(|| {
            read_error("Gateway session listing contains an invalid or repeated identifier")
        })?;
    let running = object
        .get("running")
        .and_then(Value::as_bool)
        .ok_or_else(|| read_error("Gateway session listing contains an invalid running flag"))?;
    seen.insert(session_id.to_owned());
    Ok(SessionCandidate {
        session_id: session_id.to_owned(),
        running,
    })
}

/// Validate a `session.list` response against the requested scope and limit.
///
/// # Errors
/// The session listing or a session entry has an invalid shape or field value.
pub fn parse_listing(
    response: &Value,
    scope: SessionScope,
    limit: usize,
) -> Result<SessionDiscoveryReport, GatewayReadError> {
    if response.get("scope").and_then(Value::as_str) != Some(scope.as_str()) {
        return Err(read_error(
            "Gateway session listing returned an unexpected scope",
        ));
    }
    let rows = response
        .get("sessions")
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= limit)
        .ok_or_else(|| {
            read_error("Gateway session listing is malformed or exceeds its requested limit")
        })?;
    let mut seen = BTreeSet::new();
    let sessions = rows
        .iter()
        .map(|row| parse_row(row, &mut seen))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SessionDiscoveryReport {
        scope,
        sessions,
        limit,
    })
}

/// Send one `session.list` request and validate the response.
///
/// # Errors
/// The session listing is malformed or the injected discovery RPC fails.
pub fn list_sessions<R: GatewayRpc + ?Sized>(
    rpc: &mut R,
    scope: SessionScope,
    limit: usize,
) -> Result<SessionDiscoveryReport, SessionDiscoveryError> {
    let params = BTreeMap::from([
        ("scope".to_owned(), Value::from(scope.as_str())),
        ("limit".to_owned(), Value::from(limit)),
    ]);
    let response = rpc.rpc("session.list", &params)?;
    Ok(parse_listing(&response, scope, limit)?)
}

/// List authorized sessions only; propagate RPC failures without retrying.
///
/// # Errors
/// The session listing is malformed or the injected discovery RPC fails.
pub fn discover_existing_sessions<E, R: GatewayRpc>(
    config: &GatewayAttachmentConfig<'_, E, R>,
    scope: SessionScope,
    limit: usize,
) -> Result<SessionDiscoveryReport, SessionDiscoveryError> {
    if !(1..=MAX_SESSION_SAMPLE).contains(&limit) {
        return Err(SessionDiscoveryError::Value(
            "limit must be an integer from 1 through 20".to_owned(),
        ));
    }
    attach_existing_gateway(config, |rpc| list_sessions(rpc, scope, limit))?
}

impl std::fmt::Display for SessionScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
