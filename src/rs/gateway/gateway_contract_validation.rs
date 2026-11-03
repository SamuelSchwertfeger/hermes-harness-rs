//! Read-only validation of an already-running Hermes gateway RPC contract.
//!
//! Only ping, session.list, session.info and session.events.since are permitted.
//! No runtime is started and no session is created or resumed.

use crate::util::PyStr as _;
use std::collections::BTreeMap;

use serde_json::Value;

use super::gateway_attachment::{
    GatewayAttachmentConfig, GatewayAttachmentError, attach_existing_gateway,
};
use super::gateway_readonly::{GatewayReadError, GatewayRpc, read_session};

/// Errors raised by contract validation.
#[derive(Debug, thiserror::Error)]
pub enum ContractValidationError {
    /// The gateway response violated the read-only contract.
    #[error(transparent)]
    Read(#[from] GatewayReadError),
    /// The existing gateway could not be attached.
    #[error(transparent)]
    Attachment(#[from] GatewayAttachmentError),
    /// Invalid argument (Python `ValueError`).
    #[error("{0}")]
    Value(String),
}

#[allow(clippy::struct_excessive_bools)] // mirrors the Python report
/// Non-sensitive shape-only validation result; no session data is retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayContractReport {
    /// Ping answered with the expected shape.
    pub ping_verified: bool,
    /// Number of live sessions sampled.
    pub sampled_live_session_count: usize,
    /// `session.info` answered with the expected shape.
    pub info_verified: bool,
    /// A replay epoch was observed.
    pub replay_epoch_present: bool,
    /// Latest replay sequence, if checked.
    pub replay_latest_seq: Option<u64>,
    /// The second replay request agreed with the first.
    pub replay_consistent: bool,
}

fn read_error(msg: &str) -> GatewayReadError {
    GatewayReadError::Read(msg.to_owned())
}

/// Ping must answer `{"pong": true}`.
///
/// # Errors
/// The ping result does not match the required gateway contract.
pub fn check_ping(response: &Value) -> Result<(), GatewayReadError> {
    if response.get("pong") == Some(&Value::Bool(true)) {
        Ok(())
    } else {
        Err(read_error(
            "Gateway ping response does not match Hermes contract",
        ))
    }
}

/// Validate the live listing shape and return the sampled count.
///
/// # Errors
/// The session listing does not have the required response shape.
pub fn count_sessions(response: &Value) -> Result<usize, GatewayReadError> {
    if response.get("scope").and_then(Value::as_str) != Some("live") {
        return Err(read_error("Gateway session.list returned an invalid scope"));
    }
    match response.get("sessions").and_then(Value::as_array) {
        Some(sessions) if sessions.iter().all(Value::is_object) => Ok(sessions.len()),
        _ => Err(read_error(
            "Gateway session.list returned an invalid sessions collection",
        )),
    }
}

/// Validate a replay envelope without retaining or exporting event payloads.
///
/// # Errors
/// The replay response lacks valid epoch, sequence, or completeness metadata.
pub fn replay_metadata(response: &Value) -> Result<(String, u64), GatewayReadError> {
    if !response.is_object() {
        return Err(read_error("Gateway replay metadata must be an object"));
    }
    let epoch = response
        .get("replay_epoch")
        .and_then(Value::as_str)
        .filter(|epoch| !epoch.py_strip().is_empty())
        .ok_or_else(|| read_error("Gateway replay epoch is missing"))?;
    let latest = response
        .get("latest_seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| read_error("Gateway replay sequence is invalid"))?;
    if response.get("epoch").and_then(Value::as_str) != Some(epoch) {
        return Err(read_error("Gateway replay epoch fields disagree"));
    }
    if !response.get("truncated").is_some_and(Value::is_boolean) {
        return Err(read_error("Gateway replay truncation flag is invalid"));
    }
    if !response
        .get("snapshot_required")
        .is_some_and(Value::is_boolean)
    {
        return Err(read_error("Gateway replay snapshot flag is invalid"));
    }
    if !response.get("events").is_some_and(Value::is_array) {
        return Err(read_error("Gateway replay events collection is invalid"));
    }
    Ok((epoch.to_owned(), latest))
}

/// Check the same-epoch watermark without examining any event contents.
///
/// # Errors
/// The replay watermark is inconsistent with the expected epoch or sequence.
pub fn check_replay_watermark(
    response: &Value,
    epoch: &str,
    latest: u64,
) -> Result<bool, GatewayReadError> {
    let (observed_epoch, observed_latest) = replay_metadata(response)?;
    if observed_epoch != epoch || observed_latest < latest {
        return Err(read_error(
            "Gateway replay epoch or sequence changed during validation",
        ));
    }
    let set = |key: &str| response.get(key) == Some(&Value::Bool(true));
    if set("truncated") || set("snapshot_required") {
        return Err(read_error("Gateway replay requires a fresh snapshot"));
    }
    // The second request starts at the first response's latest sequence.
    // New events may arrive concurrently; do not assert an empty result.
    Ok(true)
}

fn params<const N: usize>(pairs: [(&str, Value); N]) -> BTreeMap<String, Value> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// Perform bounded, single-shot reads without printing payloads or retrying.
///
/// # Errors
/// The gateway response violates the required contract or the injected RPC fails.
pub fn validate_rpc<R: GatewayRpc + ?Sized>(
    rpc: &mut R,
    session_id: Option<&str>,
) -> Result<GatewayContractReport, ContractValidationError> {
    let read = |error| ContractValidationError::Read(GatewayReadError::Rpc(error));
    check_ping(&rpc.rpc("ping", &BTreeMap::new()).map_err(read)?)?;
    let list = params([("limit", 1.into()), ("scope", "live".into())]);
    let count = count_sessions(&rpc.rpc("session.list", &list).map_err(read)?)?;
    let Some(session_id) = session_id else {
        return Ok(GatewayContractReport {
            ping_verified: true,
            sampled_live_session_count: count,
            info_verified: false,
            replay_epoch_present: false,
            replay_latest_seq: None,
            replay_consistent: false,
        });
    };
    read_session(rpc, session_id)?;
    let first = params([
        ("session_id", session_id.into()),
        ("last_sequence", 0.into()),
    ]);
    let metadata = rpc.rpc("session.events.since", &first).map_err(read)?;
    let (epoch, latest) = replay_metadata(&metadata)?;
    let second = params([
        ("session_id", session_id.into()),
        ("last_sequence", latest.into()),
        ("replay_epoch", epoch.as_str().into()),
    ]);
    let verified = rpc.rpc("session.events.since", &second).map_err(read)?;
    let consistent = check_replay_watermark(&verified, &epoch, latest)?;
    Ok(GatewayContractReport {
        ping_verified: true,
        sampled_live_session_count: count,
        info_verified: true,
        replay_epoch_present: true,
        replay_latest_seq: Some(latest),
        replay_consistent: consistent,
    })
}

/// Inspect an existing gateway, optionally checking one *known* live session.
///
/// Caller injects Hermes `discover_gateway_endpoint`, `attachable`, and `connect_gateway`.
/// Never select a session from the listing automatically. Do not log session metadata.
///
/// # Errors
/// The gateway response violates the required contract or the injected RPC fails.
pub fn validate_existing_gateway<E, R: GatewayRpc>(
    config: &GatewayAttachmentConfig<'_, E, R>,
    session_id: Option<&str>,
) -> Result<GatewayContractReport, ContractValidationError> {
    if session_id.is_some_and(|id| id.py_strip().is_empty()) {
        return Err(ContractValidationError::Value(
            "session_id must be a nonblank string".to_owned(),
        ));
    }
    attach_existing_gateway(config, |rpc| validate_rpc(rpc, session_id))?
}
