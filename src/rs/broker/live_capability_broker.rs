//! Bounded, single-use Unix socket capability broker; no provider adapter by default.
//!
//! A broker process runs outside Bubblewrap. Only the broker can access its gateway
//! adapter. The sandbox sees one explicitly mounted socket and a one-shot capability.

use std::collections::BTreeMap;
use std::io;
#[cfg(unix)]
use std::io::{Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::sandbox::live_sandbox_plan::Role;
#[cfg(unix)]
use crate::util::canonical_json;
use crate::util::create_private_dir_all;
use crate::util::{JsonError, random_hex, sha256_hex, sorted_json_spaced};

/// Largest accepted request frame.
pub const MAX_MESSAGE_BYTES: usize = 16_384;
/// Largest response frame.
pub const MAX_RESPONSE_BYTES: usize = 16_384;
/// Largest prompt text in bytes.
pub const MAX_TEXT_BYTES: usize = 8_192;
/// Capability token size in bytes.
pub const TOKEN_BYTES: usize = 32;
/// Length of a SHA-256 hex digest.
pub const SHA256_HEX_LENGTH: usize = 64;

/// Error returned by an injected gateway adapter.
pub type AdapterError = Box<dyn std::error::Error + Send + Sync>;

/// Request cannot be safely admitted or executed.
#[derive(Debug, thiserror::Error)]
pub enum BrokerDeniedError {
    /// The request was refused.
    #[error("{0}")]
    Denied(String),
    /// Failure raised by an injected adapter; propagated unchanged.
    #[error("{0}")]
    Adapter(#[source] AdapterError),
    /// Database failure (including UNIQUE violations on issue).
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem or socket failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

impl BrokerDeniedError {
    fn denied(message: &str) -> Self {
        Self::Denied(message.to_owned())
    }

    /// True when Python's peer handler would swallow this error (`BrokerDeniedError`,
    /// `OSError` or `ValueError`) instead of terminating the listener.
    #[must_use]
    pub fn is_peer_rejection(&self) -> bool {
        match self {
            Self::Denied(_) | Self::Io(_) => true,
            Self::Adapter(inner) => inner.downcast_ref::<io::Error>().is_some(),
            Self::Sqlite(_) | Self::Encode(_) => false,
        }
    }
}

/// One exact authorized invocation; role text is not agent-controlled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRequest {
    /// Task identity.
    pub task_id: String,
    /// `worker` or `reviewer`.
    pub role: Role,
    /// Gateway session identity.
    pub session_id: String,
    /// Client-generated submission identity.
    pub submission_id: String,
    /// RPC method; only `prompt.submit` may be issued.
    pub method: String,
    /// Lowercase SHA-256 of the exact prompt text.
    pub text_sha256: String,
}

/// Constant-time equality of two byte strings (length differences are not hidden).
#[must_use]
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let diff = left
        .iter()
        .zip(right)
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b));
    std::hint::black_box(diff) == 0
}

/// Python `str.strip()` emptiness: true when every character is Python whitespace.
#[must_use]
pub fn is_blank(text: &str) -> bool {
    crate::util::is_py_blank(text)
}

const fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Python `bytes.fromhex`: whitespace is allowed between byte pairs only.
fn from_hex(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut bytes = text.bytes().peekable();
    loop {
        while bytes
            .next_if(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
            .is_some()
        {}
        let Some(high) = bytes.next() else {
            return Some(out);
        };
        let low = bytes.next()?;
        out.push((hex_digit(high)? << 4) | hex_digit(low)?);
    }
}

/// Atomic one-time reservation and no-replay journal (no cleartext secrets).
#[derive(Debug)]
pub struct CapabilityLedger {
    /// Ledger database path.
    pub path: PathBuf,
}

impl CapabilityLedger {
    /// Open (creating if needed) the ledger at `path`.
    ///
    /// # Errors
    /// The ledger path is a symlink, or database initialization or permission setup fails.
    pub fn new(path: &Path) -> Result<Self, BrokerDeniedError> {
        if crate::util::lstat(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(BrokerDeniedError::denied(
                "Capability database must not be a symlink",
            ));
        }
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent)?;
        }
        let ledger = Self {
            path: path.to_path_buf(),
        };
        ledger.connect()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS capabilities (
                token_hash TEXT PRIMARY KEY, task_id TEXT NOT NULL,
                role TEXT NOT NULL, session_id TEXT NOT NULL,
                submission_id TEXT NOT NULL UNIQUE, method TEXT NOT NULL,
                text_sha256 TEXT NOT NULL, state TEXT NOT NULL,
                result_json TEXT
            )",
        )?;
        Ok(ledger)
    }

    fn connect(&self) -> Result<Connection, rusqlite::Error> {
        let db = crate::util::open_sqlite(&self.path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.pragma_update(None, "synchronous", "FULL")?;
        Ok(db)
    }

    /// Operator-only issuance; callers must separately authorize the task.
    ///
    /// # Errors
    /// The method or request binding is invalid, random token generation fails, or ledger insertion
    /// fails.
    pub fn issue(&self, request: &BoundRequest) -> Result<String, BrokerDeniedError> {
        if request.method != "prompt.submit" {
            return Err(BrokerDeniedError::denied(
                "Only a bounded role-specific prompt submission may be issued",
            ));
        }
        if request.task_id.is_empty()
            || request.session_id.is_empty()
            || request.submission_id.is_empty()
        {
            return Err(BrokerDeniedError::denied("Empty identity field"));
        }
        if request.text_sha256.len() != SHA256_HEX_LENGTH
            || !request
                .text_sha256
                .chars()
                .all(|c| matches!(c, '0'..='9' | 'a'..='f'))
        {
            return Err(BrokerDeniedError::denied(
                "Expected lowercase SHA256 digest",
            ));
        }
        let token = random_hex(TOKEN_BYTES)?;
        let digest = token_digest(&token)?;
        self.connect()?.execute(
            "INSERT INTO capabilities VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)",
            params![
                digest,
                request.task_id,
                request.role.as_str(),
                request.session_id,
                request.submission_id,
                request.method,
                request.text_sha256,
                "issued",
            ],
        )?;
        Ok(token)
    }

    /// Durably consume before any external side effect.
    ///
    /// # Errors
    /// The capability or request is invalid, already consumed, or its ledger cannot be accessed.
    pub fn reserve(
        &self,
        token: &str,
        data: &BTreeMap<String, String>,
    ) -> Result<BoundRequest, BrokerDeniedError> {
        let digest = token_digest(token)?;
        let mut db = self.connect()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = tx
            .query_row(
                "SELECT task_id, role, session_id, submission_id, method, text_sha256, state \
                 FROM capabilities WHERE token_hash = ?",
                [&digest],
                |row| {
                    Ok((
                        BoundRequest {
                            task_id: row.get(0)?,
                            role: Role::Worker,
                            session_id: row.get(2)?,
                            submission_id: row.get(3)?,
                            method: row.get(4)?,
                            text_sha256: row.get(5)?,
                        },
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(1)?,
                    ))
                },
            )
            .optional()?;
        let Some((mut expected, _, role)) = row.filter(|(_, state, _)| state == "issued") else {
            return Err(BrokerDeniedError::denied(
                "Capability unknown or already consumed",
            ));
        };
        expected.role = match role.as_str() {
            "worker" => Role::Worker,
            "reviewer" => Role::Reviewer,
            _ => {
                return Err(BrokerDeniedError::denied(
                    "Only a bounded role-specific prompt submission may be issued",
                ));
            }
        };
        validate_bound_request(&expected, data)?;
        tx.execute(
            "UPDATE capabilities SET state = 'submitted_uncertain' WHERE token_hash = ?",
            [&digest],
        )?;
        tx.commit()?;
        Ok(expected)
    }

    /// Record the (safe) response of a reserved capability.
    ///
    /// # Errors
    /// The token is malformed, response encoding fails, or the ledger update fails.
    pub fn complete(
        &self,
        token: &str,
        response: &BTreeMap<&str, &str>,
    ) -> Result<(), BrokerDeniedError> {
        let digest = token_digest(token)?;
        self.connect()?.execute(
            "UPDATE capabilities SET state = 'response_recorded', result_json = ? \
             WHERE token_hash = ? AND state = 'submitted_uncertain'",
            params![sorted_json_spaced(response)?, digest],
        )?;
        Ok(())
    }

    /// Ledger state of a token, or `None` when malformed or unknown.
    ///
    /// # Errors
    /// The capability ledger cannot be opened or queried.
    pub fn status(&self, token: &str) -> Result<Option<String>, BrokerDeniedError> {
        let Some(raw) = from_hex(token) else {
            return Ok(None);
        };
        let digest = sha256_hex(&raw);
        Ok(self
            .connect()?
            .query_row(
                "SELECT state FROM capabilities WHERE token_hash = ?",
                [&digest],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }
}

/// SHA-256 hex of the raw token bytes; rejects malformed tokens.
///
/// # Errors
/// The token is not valid hexadecimal of the required length.
fn token_digest(token: &str) -> Result<String, BrokerDeniedError> {
    let raw = from_hex(token).ok_or_else(|| BrokerDeniedError::denied("Malformed capability"))?;
    if raw.len() != TOKEN_BYTES {
        return Err(BrokerDeniedError::denied("Malformed capability length"));
    }
    Ok(sha256_hex(&raw))
}

/// Check the request fields against the reserved binding and the prompt digest.
///
/// # Errors
/// Request identity fields, prompt size, or prompt digest do not match the reserved binding.
fn validate_bound_request(
    expected: &BoundRequest,
    data: &BTreeMap<String, String>,
) -> Result<(), BrokerDeniedError> {
    let fields = [
        ("task_id", expected.task_id.as_str()),
        ("role", expected.role.as_str()),
        ("session_id", expected.session_id.as_str()),
        ("submission_id", expected.submission_id.as_str()),
        ("method", expected.method.as_str()),
    ];
    if fields
        .iter()
        .any(|(name, value)| data.get(*name).map(String::as_str) != Some(*value))
    {
        return Err(BrokerDeniedError::denied("Capability identity mismatch"));
    }
    let text = data.get("text").map(String::as_str);
    let Some(text) = text.filter(|t| !is_blank(t) && t.len() <= MAX_TEXT_BYTES) else {
        return Err(BrokerDeniedError::denied("Invalid or oversized prompt"));
    };
    if !constant_time_eq(
        sha256_hex(text.as_bytes()).as_bytes(),
        expected.text_sha256.as_bytes(),
    ) {
        return Err(BrokerDeniedError::denied("Prompt digest mismatch"));
    }
    Ok(())
}

/// Adapter taking an RPC method and parameters.
pub type GatewayAdapter = Box<
    dyn FnMut(&str, &BTreeMap<String, String>) -> Result<Map<String, Value>, AdapterError> + Send,
>;

/// Adapter taking the exact authorized request and prompt text.
pub type BoundAdapter =
    Box<dyn FnMut(&BoundRequest, &str) -> Result<Map<String, Value>, AdapterError> + Send>;

/// The only data returned to a sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrokerResponse {
    /// Gateway admission identity.
    pub admission_id: String,
}

/// Never trusts an agent-specified gateway destination or RPC operation.
pub struct CapabilityBroker {
    /// Reservation ledger.
    pub ledger: CapabilityLedger,
    adapter: Option<GatewayAdapter>,
    bound_adapter: Option<BoundAdapter>,
}

impl std::fmt::Debug for CapabilityBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapabilityBroker")
            .field("ledger", &self.ledger)
            .finish_non_exhaustive()
    }
}

impl CapabilityBroker {
    /// Build a broker with an optional method/params adapter.
    #[must_use]
    pub const fn new(ledger: CapabilityLedger, adapter: Option<GatewayAdapter>) -> Self {
        Self {
            ledger,
            adapter,
            bound_adapter: None,
        }
    }

    /// Build a broker with a bound-request adapter (takes precedence over a plain adapter).
    #[must_use]
    pub fn with_bound_adapter(ledger: CapabilityLedger, bound_adapter: BoundAdapter) -> Self {
        Self {
            ledger,
            adapter: None,
            bound_adapter: Some(bound_adapter),
        }
    }

    /// Decode, reserve, invoke the adapter once, and record the safe response.
    ///
    /// # Errors
    /// The capability or request is invalid, already consumed, or its ledger cannot be accessed.
    pub fn handle(&mut self, payload: &[u8]) -> Result<BrokerResponse, BrokerDeniedError> {
        let data = decode_request(payload)?;
        let token = data.get("capability").cloned().unwrap_or_default();
        if self.adapter.is_none() && self.bound_adapter.is_none() {
            return Err(BrokerDeniedError::denied(
                "No qualified gateway adapter configured",
            ));
        }
        let authorized = self.ledger.reserve(&token, &data)?;
        // Reservation precedes all adapter activity; never auto-retry failures.
        let text = data.get("text").map_or("", String::as_str);
        let response = self.invoke(&authorized, text)?;
        let Some(Value::String(admission_id)) = response.get("admission_id") else {
            return Err(BrokerDeniedError::denied(
                "Gateway admission response invalid; outcome uncertain",
            ));
        };
        self.ledger.complete(
            &token,
            &BTreeMap::from([("admission_id", admission_id.as_str())]),
        )?;
        Ok(BrokerResponse {
            admission_id: admission_id.clone(),
        })
    }

    fn invoke(
        &mut self,
        authorized: &BoundRequest,
        text: &str,
    ) -> Result<Map<String, Value>, BrokerDeniedError> {
        if let Some(bound) = self.bound_adapter.as_mut() {
            return bound(authorized, text).map_err(BrokerDeniedError::Adapter);
        }
        if let Some(adapter) = self.adapter.as_mut() {
            let params = BTreeMap::from([
                ("session_id".to_owned(), authorized.session_id.clone()),
                ("submission_id".to_owned(), authorized.submission_id.clone()),
                ("text".to_owned(), text.to_owned()),
            ]);
            return adapter(&authorized.method, &params).map_err(BrokerDeniedError::Adapter);
        }
        Err(BrokerDeniedError::denied("No adapter configured"))
    }
}

/// Decode a socket request without ledger or adapter side effects.
///
/// # Errors
/// Returns the same validation errors as the broker request decoder.
#[doc(hidden)]
pub fn decode_request_for_fuzzing(
    payload: &[u8],
) -> Result<BTreeMap<String, String>, BrokerDeniedError> {
    decode_request(payload)
}

/// Parse and strictly validate one request payload.
///
/// # Errors
/// The JSON frame has an invalid shape, fields, or field types.
fn decode_request(payload: &[u8]) -> Result<BTreeMap<String, String>, BrokerDeniedError> {
    if payload.len() > MAX_MESSAGE_BYTES {
        return Err(BrokerDeniedError::denied("Request too large"));
    }
    let data: Value = serde_json::from_slice(payload)
        .map_err(|_| BrokerDeniedError::denied("Invalid JSON request"))?;
    let fields = [
        "capability",
        "task_id",
        "role",
        "session_id",
        "submission_id",
        "method",
        "text",
    ];
    let Value::Object(map) = data else {
        return Err(BrokerDeniedError::denied("Unexpected request fields"));
    };
    if map.len() != fields.len() || !fields.iter().all(|f| map.contains_key(*f)) {
        return Err(BrokerDeniedError::denied("Unexpected request fields"));
    }
    map.into_iter()
        .map(|(key, value)| match value {
            Value::String(text) => Ok((key, text)),
            _ => Err(BrokerDeniedError::denied("Request values must be strings")),
        })
        .collect()
}

/// UID of the connected peer, read with `SO_PEERCRED`.
///
/// Fails closed: where `SO_PEERCRED` is unavailable (non-Linux unix) the peer is
/// reported as unknown and the connection is denied.
#[cfg(unix)]
fn peer_uid(conn: &UnixStream) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        rustix::net::sockopt::get_socket_peercred(conn)
            .ok()
            .map(|cred| cred.uid.as_raw())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = conn;
        None
    }
}

/// Serve exactly one framed request from an authorized same-host UID.
///
/// # Errors
/// Peer credentials, request framing, socket I/O, or broker request handling fails.
#[cfg(unix)]
pub fn serve_connection(
    conn: &mut UnixStream,
    broker: &mut CapabilityBroker,
    expected_uid: u32,
) -> Result<(), BrokerDeniedError> {
    conn.set_read_timeout(Some(Duration::from_secs(5)))?;
    conn.set_write_timeout(Some(Duration::from_secs(5)))?;
    if peer_uid(conn) != Some(expected_uid) {
        return Err(BrokerDeniedError::denied("Peer UID not authorized"));
    }
    let header = receive(conn, 4)?;
    let count = header
        .iter()
        .fold(0_usize, |acc, byte| (acc << 8) | usize::from(*byte));
    if !(1..=MAX_MESSAGE_BYTES).contains(&count) {
        return Err(BrokerDeniedError::denied("Frame exceeds limit"));
    }
    let payload = receive(conn, count)?;
    let result = canonical_json(&broker.handle(&payload)?)?.into_bytes();
    if result.len() > MAX_RESPONSE_BYTES {
        return Err(BrokerDeniedError::denied("Response exceeds limit"));
    }
    let length = u32::try_from(result.len())
        .map_err(|_| BrokerDeniedError::denied("Response exceeds limit"))?;
    let mut frame = length.to_be_bytes().to_vec();
    frame.extend_from_slice(&result);
    conn.write_all(&frame)?;
    Ok(())
}

#[cfg(unix)]
fn receive(conn: &mut UnixStream, count: usize) -> Result<Vec<u8>, BrokerDeniedError> {
    let mut result = vec![0_u8; count];
    let mut filled = 0;
    while filled < count {
        let Some(window) = result.get_mut(filled..) else {
            break;
        };
        match conn.read(window) {
            Ok(0) => {
                return Err(BrokerDeniedError::denied("Socket closed before full frame"));
            }
            Ok(read) => filled += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(result)
}
