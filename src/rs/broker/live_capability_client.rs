//! Single-request Unix socket client for pre-authorized sandbox invocation.
#![cfg(unix)]

use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::Value;

use super::live_capability_broker::{MAX_MESSAGE_BYTES, MAX_RESPONSE_BYTES, is_blank};
use crate::util::ordered_map::OrderedMap;
use crate::util::{JsonError, ordered_json_spaced};

/// The broker response cannot be trusted or submission is uncertain.
#[derive(Debug, thiserror::Error)]
pub enum CapabilityClientError {
    /// Untrusted response or uncertain outcome.
    #[error("{0}")]
    Untrusted(String),
    /// Communication failure with its underlying cause.
    #[error("{message}")]
    Communication {
        /// Unchanged uncertainty message.
        message: &'static str,
        /// Transport or decoding failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

fn untrusted(message: &str) -> CapabilityClientError {
    CapabilityClientError::Untrusted(message.to_owned())
}

fn uncertain(error: impl std::error::Error + Send + Sync + 'static) -> CapabilityClientError {
    CapabilityClientError::Communication {
        message: "Broker communication outcome uncertain; do not retry",
        source: Box::new(error),
    }
}

/// Send one request, without any transport retry or configurable RPC method.
///
/// `request` keeps its insertion order on the wire, like a Python dict.
///
/// # Errors
/// Request encoding, socket communication, or admission response validation fails; no retry occurs.
pub fn submit_once(
    socket_path: &str,
    request: &OrderedMap<String>,
) -> Result<String, CapabilityClientError> {
    let raw = compact(request)?;
    if raw.is_empty() || raw.len() > MAX_MESSAGE_BYTES {
        return Err(untrusted("Capability request exceeds maximum frame size"));
    }
    let decoded = exchange(socket_path, &raw)?;
    validate_admission(&decoded)
}

/// `json.dumps(request, separators=(",", ":"))` in insertion order.
fn compact(request: &OrderedMap<String>) -> Result<Vec<u8>, CapabilityClientError> {
    let mut out = String::from("{");
    for (index, (key, item)) in request.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&ordered_json_spaced(key)?);
        out.push(':');
        out.push_str(&ordered_json_spaced(item)?);
    }
    out.push('}');
    Ok(out.into_bytes())
}

fn exchange(socket_path: &str, raw: &[u8]) -> Result<Value, CapabilityClientError> {
    let mut conn = UnixStream::connect(socket_path).map_err(uncertain)?;
    conn.set_read_timeout(Some(Duration::from_secs(5)))
        .and_then(|()| conn.set_write_timeout(Some(Duration::from_secs(5))))
        .map_err(uncertain)?;
    let length = u32::try_from(raw.len()).map_err(uncertain)?;
    let mut frame = length.to_be_bytes().to_vec();
    frame.extend_from_slice(raw);
    conn.write_all(&frame).map_err(uncertain)?;
    let header = receive(&mut conn, 4)?;
    let length = header
        .iter()
        .fold(0_usize, |acc, byte| (acc << 8) | usize::from(*byte));
    if !(1..=MAX_RESPONSE_BYTES).contains(&length) {
        return Err(untrusted(
            "Invalid broker response length; outcome uncertain",
        ));
    }
    let body = receive(&mut conn, length)?;
    serde_json::from_slice(&body).map_err(uncertain)
}

fn receive(conn: &mut UnixStream, count: usize) -> Result<Vec<u8>, CapabilityClientError> {
    let mut pieces = vec![0_u8; count];
    let mut filled = 0;
    while filled < count {
        let Some(window) = pieces.get_mut(filled..) else {
            break;
        };
        match conn.read(window) {
            Ok(0) => {
                return Err(untrusted("Incomplete broker response; outcome uncertain"));
            }
            Ok(read) => filled += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(uncertain(error)),
        }
    }
    Ok(pieces)
}

/// Require exactly `{"admission_id": <nonblank string>}`.
///
/// # Errors
/// The response is not an object containing exactly one nonblank admission ID.
fn validate_admission(decoded: &Value) -> Result<String, CapabilityClientError> {
    let Value::Object(map) = decoded else {
        return Err(untrusted("Invalid broker admission; outcome uncertain"));
    };
    if map.len() != 1 || !map.contains_key("admission_id") {
        return Err(untrusted("Invalid broker admission; outcome uncertain"));
    }
    match map.get("admission_id") {
        Some(Value::String(admission)) if !is_blank(admission) => Ok(admission.clone()),
        _ => Err(untrusted("Blank broker admission; outcome uncertain")),
    }
}
