//! Read-only pending-admission identity recovery from a replay-derived snapshot.
//!
//! A snapshot is never evidence of absence or completed execution. No RPC is sent
//! by this module; a caller supplies a previously authenticated `SessionSnapshot`.

use crate::util::PyStr as _;
use std::fmt;

use serde_json::{Map, Value};

use super::gateway_readonly::SessionSnapshot;

/// Conservative classification of a snapshot's matching pending entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PendingDisposition {
    /// Exactly one valid match.
    Found,
    /// Anything else.
    Indeterminate,
}

impl PendingDisposition {
    /// The string value Python's `StrEnum` uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Found => "found",
            Self::Indeterminate => "indeterminate",
        }
    }
}

impl fmt::Display for PendingDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A candidate admission identity, not a completion or cancellation receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingLookup {
    /// Classification.
    pub disposition: PendingDisposition,
    /// Candidate admission identity when found.
    pub admission_id: Option<String>,
}

const UNKNOWN: PendingLookup = PendingLookup {
    disposition: PendingDisposition::Indeterminate,
    admission_id: None,
};

fn matches<'a>(
    pending: Option<&'a Value>,
    submission_id: &str,
) -> Option<Vec<&'a Map<String, Value>>> {
    let items = pending?.as_array()?;
    let mut found = Vec::new();
    for item in items {
        let object = item.as_object()?;
        if object.get("input_id").and_then(Value::as_str) != Some(submission_id) {
            continue;
        }
        found.push(object);
    }
    Some(found)
}

/// Extract exactly one matching pending admission from an existing snapshot.
///
/// Fail closed on malformed snapshots and multiple matches. An empty pending
/// list provides no proof of non-admission: terminal work may be absent.
#[must_use]
pub fn inspect_pending(
    snapshot: &SessionSnapshot,
    session_id: &str,
    submission_id: &str,
) -> PendingLookup {
    if session_id.py_strip().is_empty()
        || submission_id.py_strip().is_empty()
        || snapshot.session_id != session_id
    {
        return UNKNOWN;
    }
    let found = matches(snapshot.data.get("pending"), submission_id);
    let Some([only]) = found.as_deref() else {
        return UNKNOWN;
    };
    match only.get("admission_id").and_then(Value::as_str) {
        Some(admission_id) if !admission_id.py_strip().is_empty() => PendingLookup {
            disposition: PendingDisposition::Found,
            admission_id: Some(admission_id.to_owned()),
        },
        _ => UNKNOWN,
    }
}
