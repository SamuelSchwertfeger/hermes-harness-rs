//! Read a complete, epoch-bound replay to recover a pending-admission candidate.
//!
//! Hermes' `session.info` RPC is a session metadata projection; the pending list is
//! published in `session.info` events. This module uses only the read-only
//! `session.events.since` RPC and refuses truncated or inconsistent replays.

use crate::util::PyStr as _;
use std::collections::BTreeMap;

use serde_json::{Map, Value};

pub use super::gateway_readonly::GatewayRpc as EventReplayRpc;
use super::gateway_readonly::{GatewayReadError, GatewayRpc, GatewayRpcError, SessionSnapshot};

struct ReplayEnvelope {
    epoch: String,
    latest_seq: i128,
    truncated: bool,
    snapshot_required: bool,
    events: Vec<Value>,
}

struct ReplayContext<'a> {
    session_id: &'a str,
    epoch: &'a str,
    latest_seq: i128,
    previous_seq: i128,
}

fn read_err(msg: &str) -> GatewayReadError {
    GatewayReadError::Read(msg.to_owned())
}

/// Python `type(x) is int`: JSON integers only (booleans and floats excluded).
fn int_of(value: Option<&Value>) -> Option<i128> {
    let Value::Number(number) = value? else {
        return None;
    };
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

/// Validate the stable envelope returned by `session.events.since`.
fn replay_fields(response: Value) -> Result<ReplayEnvelope, GatewayReadError> {
    let Value::Object(mut object) = response else {
        return Err(read_err(
            "session.events.since must return an object with string keys",
        ));
    };
    let invalid = || read_err("session.events.since returned an invalid replay envelope");

    let epoch = object
        .get("replay_epoch")
        .and_then(Value::as_str)
        .filter(|text| !text.py_strip().is_empty())
        .filter(|text| object.get("epoch").and_then(Value::as_str) == Some(*text))
        .ok_or_else(invalid)?
        .to_owned();
    let latest_seq = int_of(object.get("latest_seq"))
        .filter(|seq| *seq >= 0)
        .ok_or_else(invalid)?;
    let truncated = object
        .get("truncated")
        .and_then(Value::as_bool)
        .ok_or_else(invalid)?;
    let snapshot_required = object
        .get("snapshot_required")
        .and_then(Value::as_bool)
        .ok_or_else(invalid)?;
    let events = object
        .remove("events")
        .and_then(|value| match value {
            Value::Array(events) => Some(events),
            _ => None,
        })
        .ok_or_else(invalid)?;
    Ok(ReplayEnvelope {
        epoch,
        latest_seq,
        truncated,
        snapshot_required,
        events,
    })
}

/// Validate one replay event and return its sequence, type, and payload.
fn event_fields<'a>(
    event: &'a Value,
    context: &ReplayContext<'_>,
) -> Result<(i128, &'a str, &'a Map<String, Value>), GatewayReadError> {
    let Some(object) = event.as_object() else {
        return Err(read_err("session.events.since returned a malformed event"));
    };
    let inconsistent = || read_err("session.events.since returned an inconsistent event");

    let sequence = int_of(object.get("seq")).ok_or_else(inconsistent)?;
    if sequence <= context.previous_seq || sequence > context.latest_seq {
        return Err(inconsistent());
    }
    let same_session = object.get("session_id").and_then(Value::as_str) == Some(context.session_id);
    let same_epoch = object.get("replay_epoch").and_then(Value::as_str) == Some(context.epoch);
    if !same_session || !same_epoch {
        return Err(inconsistent());
    }
    let event_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(inconsistent)?;
    let payload = object
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(inconsistent)?;
    Ok((sequence, event_type, payload))
}

/// Return the newest pending snapshot, refusing malformed or absent snapshots.
fn latest_pending<'a>(
    replay: &'a ReplayEnvelope,
    session_id: &str,
) -> Result<Option<&'a [Value]>, GatewayReadError> {
    let mut latest = None;
    let mut context = ReplayContext {
        session_id,
        epoch: &replay.epoch,
        latest_seq: replay.latest_seq,
        previous_seq: 0,
    };
    for event in &replay.events {
        let (sequence, event_type, payload) = event_fields(event, &context)?;
        if sequence != context.previous_seq + 1 {
            return Ok(None);
        }
        context.previous_seq = sequence;
        if event_type != "session.info" {
            continue;
        }
        let Some(pending) = payload.get("pending") else {
            continue;
        };
        let Some(list) = pending.as_array() else {
            return Ok(None);
        };
        latest = Some(list.as_slice());
    }
    if context.previous_seq != replay.latest_seq {
        return Ok(None);
    }
    Ok(latest)
}

fn since<R: GatewayRpc + ?Sized>(
    rpc: &mut R,
    session_id: &str,
    epoch: Option<&str>,
) -> Result<ReplayEnvelope, GatewayReadError> {
    let mut params = BTreeMap::from([
        ("session_id".to_owned(), Value::from(session_id)),
        ("last_sequence".to_owned(), Value::from(0)),
    ]);
    if let Some(epoch) = epoch {
        params.insert("replay_epoch".to_owned(), Value::from(epoch));
    }
    match rpc.rpc("session.events.since", &params) {
        Ok(response) => replay_fields(response),
        Err(GatewayRpcError::Io(_)) => Err(read_err(
            "Gateway event replay failed; no retry was attempted",
        )),
        Err(other) => Err(other.into()),
    }
}

/// Return the latest replayed pending snapshot, or `None` when uncertain.
///
/// The first request learns the current replay epoch. The second requests the
/// entire retained history from sequence zero under that epoch. A gap, epoch
/// change, or missing snapshot is inconclusive, never proof of non-admission.
///
/// # Errors
/// The event replay is incomplete, inconsistent, or malformed, or the injected RPC fails.
pub fn read_pending_snapshot<R: GatewayRpc + ?Sized>(
    rpc: &mut R,
    session_id: &str,
) -> Result<Option<SessionSnapshot>, GatewayReadError> {
    if session_id.py_strip().is_empty() {
        return Err(GatewayReadError::InvalidArgument(
            "session_id must be a nonblank string".to_owned(),
        ));
    }

    let initial = since(rpc, session_id, None)?;
    if initial.latest_seq == 0 {
        return Ok(None);
    }
    let replay = since(rpc, session_id, Some(&initial.epoch))?;

    if replay.epoch != initial.epoch || replay.truncated || replay.snapshot_required {
        return Ok(None);
    }
    if replay.latest_seq < initial.latest_seq {
        return Ok(None);
    }
    let Some(pending) = latest_pending(&replay, session_id)? else {
        return Ok(None);
    };
    let mut data = Map::new();
    data.insert("pending".to_owned(), Value::Array(pending.to_vec()));
    Ok(Some(SessionSnapshot {
        session_id: session_id.to_owned(),
        data,
    }))
}
