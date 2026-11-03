//! Read-only session inspection through an explicitly injected authenticated connector.
//!
//! The harness neither imports Hermes nor discovers or starts a gateway. The caller
//! must provide a connector that attaches to an existing, authenticated endpoint.

use crate::util::PyStr as _;
use std::collections::BTreeMap;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Failure raised by an RPC call.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GatewayRpcError {
    /// Transport failure (Python `OSError`).
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Invalid argument or scripted refusal (Python `ValueError`).
    #[error("{0}")]
    Value(String),
}

/// Errors raised by the read-only gateway boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GatewayReadError {
    /// Gateway read was refused, disconnected, or returned an invalid payload.
    #[error("{0}")]
    Read(String),
    /// Caller supplied an invalid argument (Python `ValueError`).
    #[error("{0}")]
    InvalidArgument(String),
    /// Non-transport RPC failure propagated unchanged.
    #[error(transparent)]
    Rpc(#[from] GatewayRpcError),
}

/// A bounded, opaque response returned by `session.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Session identity.
    pub session_id: String,
    /// Opaque response object.
    pub data: Map<String, Value>,
}

/// Minimal already-authenticated RPC interface.
pub trait GatewayRpc {
    /// Make a gateway RPC request.
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError>;
}

fn nonblank<'a>(value: &'a str, label: &str) -> Result<&'a str, GatewayReadError> {
    if value.py_strip().is_empty() {
        return Err(GatewayReadError::InvalidArgument(format!(
            "{label} must be a nonblank string"
        )));
    }
    Ok(value)
}

/// Read metadata using an already-authenticated RPC connection; never retry.
///
/// # Errors
/// The session snapshot is malformed or inconsistent, or the injected RPC fails.
pub fn read_session<R: GatewayRpc + ?Sized>(
    rpc: &mut R,
    session_id: &str,
) -> Result<SessionSnapshot, GatewayReadError> {
    let session_id = nonblank(session_id, "session_id")?;
    let params = BTreeMap::from([("session_id".to_owned(), Value::from(session_id))]);
    let response = match rpc.rpc("session.info", &params) {
        Ok(value) => value,
        Err(GatewayRpcError::Io(_)) => {
            return Err(GatewayReadError::Read(
                "Gateway session read failed; no retry was attempted".to_owned(),
            ));
        }
        Err(other) => return Err(other.into()),
    };
    match response {
        Value::Object(data) => Ok(SessionSnapshot {
            session_id: session_id.to_owned(),
            data,
        }),
        _ => Err(GatewayReadError::Read(
            "session.info must return an object with string keys".to_owned(),
        )),
    }
}

/// Use a caller-provided authenticated connector with an explicit endpoint.
///
/// A Hermes-side caller can pass its gateway connect function after explicitly
/// discovering and validating a ready endpoint. This module never imports Hermes,
/// consults credentials, or starts the gateway itself.
///
/// # Errors
/// The session snapshot is malformed or inconsistent, or the injected RPC fails.
pub fn read_from_hermes<E, R, C>(
    endpoint: Option<E>,
    session_id: &str,
    connector: C,
) -> Result<SessionSnapshot, GatewayReadError>
where
    R: GatewayRpc,
    C: FnOnce(E) -> Result<R, GatewayRpcError>,
{
    let Some(endpoint) = endpoint else {
        return Err(GatewayReadError::InvalidArgument(
            "A pre-discovered gateway endpoint is mandatory; auto-start is forbidden".to_owned(),
        ));
    };
    nonblank(session_id, "session_id")?;
    let mut rpc = connector(endpoint)?;
    read_session(&mut rpc, session_id)
}
