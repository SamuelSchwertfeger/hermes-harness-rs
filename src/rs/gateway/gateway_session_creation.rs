//! Fail-closed, one-shot disposable session creation for an attached gateway.
//!
//! This module never discovers or starts a gateway and never submits a prompt.

use crate::util::PyStr as _;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::util::{resolve_strict, sorted_json_spaced};

/// Longest accepted `request_id`, in characters.
pub const MAX_REQUEST_ID_LENGTH: usize = 256;

/// Failure to create a session, or an outcome that cannot be trusted.
#[derive(Debug, thiserror::Error)]
pub enum SessionCreationError {
    /// Creation cannot be trusted or safely attempted.
    #[error("{0}")]
    Creation(String),
    /// Creation may have happened; never send the request again automatically.
    #[error("{0}")]
    Unknown(String),
    /// The request itself is invalid (Python `ValueError`).
    #[error("{0}")]
    InvalidRequest(String),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Serialisation failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

impl SessionCreationError {
    /// True for `SessionCreationUnknownError`: the session may exist.
    #[must_use]
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

/// Error type an RPC transport may fail with.
pub type RpcFailure = Box<dyn std::error::Error + Send + Sync>;

/// An already-authenticated, explicitly attached RPC connection.
pub trait SessionRpc {
    /// Send `method` with keyword `params` and return the parsed response.
    fn rpc(&mut self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcFailure>;
}

/// A unique durable creation identity and an operator-chosen ledger location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCreationRequest {
    /// Unique creation identity.
    pub request_id: String,
    /// Ledger file; its existence is the no-retry barrier.
    pub record_path: PathBuf,
}

/// Confirmed attached session; only valid on the RPC used for creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedSession {
    /// Local session identifier.
    pub session_id: String,
    /// Subscription proving same-connection attachment.
    pub subscription_id: String,
    /// The request that created it.
    pub request_id: String,
}

fn open_for_write(path: &Path, exclusive: bool) -> Result<fs::File, SessionCreationError> {
    let mut options = fs::OpenOptions::new();
    if exclusive {
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
    } else {
        options.write(true).truncate(true);
    }
    match options.open(path) {
        Err(err) if exclusive && err.kind() == io::ErrorKind::AlreadyExists => {
            Err(SessionCreationError::Creation(
                "Creation record already exists; do not retry the request".to_owned(),
            ))
        }
        other => Ok(other?),
    }
}

/// Create the initial no-retry barrier atomically, then sync status updates.
fn persist(
    path: &Path,
    record: &BTreeMap<String, String>,
    exclusive: bool,
) -> Result<(), SessionCreationError> {
    let mut stream = open_for_write(path, exclusive)?;
    stream.write_all(sorted_json_spaced(record)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    stream.sync_all()?;
    Ok(())
}

/// Resolve and validate a safe default working directory synchronously.
///
/// # Errors
/// The disposable working directory or session parameters are invalid, or path resolution fails.
fn disposable_cwd() -> Result<String, SessionCreationError> {
    let cwd = resolve_strict(&std::env::temp_dir())?;
    if !cwd.is_dir() {
        return Err(SessionCreationError::Creation(
            "Disposable session cwd is not a directory".to_owned(),
        ));
    }
    Ok(cwd.to_string_lossy().into_owned())
}

fn unknown(message: &str) -> SessionCreationError {
    SessionCreationError::Unknown(message.to_owned())
}

fn verify_response(
    response: &Value,
    request: &SessionCreationRequest,
) -> Result<PreparedSession, SessionCreationError> {
    let Value::Object(map) = response else {
        return Err(unknown("session.create returned an invalid response"));
    };
    let text = |key: &str| map.get(key).and_then(Value::as_str);
    let session_id = text("session_id")
        .filter(|sid| sid.starts_with("local-") && !sid.py_strip().is_empty())
        .ok_or_else(|| unknown("session.create returned no valid local session identity"))?;
    let subscription_id = text("subscription_id")
        .filter(|sub| !sub.py_strip().is_empty())
        .ok_or_else(|| unknown("session.create did not confirm attachment to this connection"))?;
    Ok(PreparedSession {
        session_id: session_id.to_owned(),
        subscription_id: subscription_id.to_owned(),
        request_id: request.request_id.clone(),
    })
}

fn validate_request_id(request_id: &str) -> Result<(), SessionCreationError> {
    if request_id.py_strip().is_empty() {
        return Err(SessionCreationError::InvalidRequest(
            "request_id must be nonblank".to_owned(),
        ));
    }
    if request_id.chars().count() > MAX_REQUEST_ID_LENGTH {
        return Err(SessionCreationError::InvalidRequest(
            "request_id exceeds Hermes limit".to_owned(),
        ));
    }
    Ok(())
}

/// Create once, requiring same-connection attachment evidence.
///
/// The exclusive record is a no-retry barrier, *not* a transaction log. A power
/// failure during an update may corrupt it; even then an existing file always
/// prevents automatic re-creation. This function does not submit a prompt.
///
/// # Errors
/// The disposable working directory or session parameters are invalid, or path resolution fails.
pub fn prepare_disposable_session(
    rpc: &mut dyn SessionRpc,
    request: &SessionCreationRequest,
) -> Result<PreparedSession, SessionCreationError> {
    validate_request_id(&request.request_id)?;
    // Never inherit the operator profile's tools or workspace. Hermes treats an
    // explicit empty toolsets list as opt-out from all toolsets (including the
    // context-engine toolset); the temporary directory is the session cwd.
    let cwd = disposable_cwd()?;
    let mut record = BTreeMap::from([
        ("request_id".to_owned(), request.request_id.clone()),
        ("status".to_owned(), "unknown".to_owned()),
    ]);
    persist(&request.record_path, &record, true)?;
    let params = json!({
        "request_id": request.request_id,
        "source": "cli",
        "cwd": cwd,
        "toolsets": [],
        "max_turns": 1,
        "run_budget": 60,
    });
    let Value::Object(params) = params else {
        return Err(unknown(
            "Session creation outcome unknown; do not retry or submit",
        ));
    };
    let prepared = rpc
        .rpc("session.create", &params)
        .map_err(|_| ())
        .and_then(|response| verify_response(&response, request).map_err(|_| ()))
        .map_err(|()| unknown("Session creation outcome unknown; do not retry or submit"))?;
    record.insert("status".to_owned(), "attached".to_owned());
    record.insert("session_id".to_owned(), prepared.session_id.clone());
    persist(&request.record_path, &record, false)
        .map_err(|_| unknown("Session created but its identity could not be durably recorded"))?;
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    #![allow(
        missing_docs,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::pedantic
    )]
    //! No-retry and subscription-evidence checks for session creation.

    use std::path::Path;

    use super::{
        RpcFailure, SessionCreationRequest, SessionRpc, disposable_cwd, prepare_disposable_session,
    };
    use serde_json::{Map, Value, json};

    const SESSION_ID: &str = "local-aaaaaaaaaaaaaaaa";
    const SUBSCRIPTION_ID: &str = "subscription-1";

    enum Outcome {
        Response(Value),
        Failure(String),
    }

    /// Count session-create requests and optionally fail before returning.
    struct StubRpc {
        result: Outcome,
        calls: Vec<(String, Map<String, Value>)>,
    }

    impl StubRpc {
        const fn new(result: Outcome) -> Self {
            Self {
                result,
                calls: Vec::new(),
            }
        }
    }

    impl SessionRpc for StubRpc {
        fn rpc(&mut self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcFailure> {
            self.calls.push((method.to_owned(), params.clone()));
            match &self.result {
                Outcome::Failure(text) => Err(text.clone().into()),
                Outcome::Response(value) => Ok(value.clone()),
            }
        }
    }

    fn request(dir: &Path) -> SessionCreationRequest {
        SessionCreationRequest {
            request_id: "m3-disposable-test".to_owned(),
            record_path: dir.join("creation.json"),
        }
    }

    fn success() -> Value {
        json!({"session_id": SESSION_ID, "subscription_id": SUBSCRIPTION_ID})
    }

    fn read_record(request: &SessionCreationRequest) -> Value {
        serde_json::from_str(&std::fs::read_to_string(&request.record_path).unwrap()).unwrap()
    }

    #[test]
    fn creation_records_attached_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rpc = StubRpc::new(Outcome::Response(success()));
        let req = request(tmp.path());
        let result = prepare_disposable_session(&mut rpc, &req).unwrap();
        assert_eq!(result.session_id, SESSION_ID);
        assert_eq!(result.subscription_id, SUBSCRIPTION_ID);
        let (method, params) = &rpc.calls[0];
        assert_eq!(method, "session.create");
        let expected = json!({
            "request_id": req.request_id,
            "source": "cli",
            "cwd": disposable_cwd().unwrap(),
            "toolsets": [],
            "max_turns": 1,
            "run_budget": 60,
        });
        assert_eq!(&Value::Object(params.clone()), &expected);
        assert_eq!(rpc.calls.len(), 1);
        assert_eq!(
            read_record(&req),
            json!({
                "request_id": req.request_id,
                "session_id": SESSION_ID,
                "status": "attached",
            })
        );
    }

    #[test]
    fn creation_cwd_is_the_resolved_temp_dir() {
        let cwd = disposable_cwd().unwrap();
        let resolved = std::env::temp_dir().canonicalize().unwrap();
        let verbatim = r"\\?\";
        assert_eq!(
            cwd.trim_start_matches(verbatim),
            resolved.to_string_lossy().trim_start_matches(verbatim)
        );
    }

    #[test]
    fn missing_attachment_evidence_fails_closed() {
        let responses = [
            Value::Null,
            json!({}),
            json!({"session_id": SESSION_ID}),
            json!({"subscription_id": "sub"}),
        ];
        for response in responses {
            let tmp = tempfile::tempdir().unwrap();
            let mut rpc = StubRpc::new(Outcome::Response(response));
            let req = request(tmp.path());
            let err = prepare_disposable_session(&mut rpc, &req).unwrap_err();
            assert!(err.is_unknown());
            assert!(err.to_string().contains("outcome unknown"));
            assert_eq!(read_record(&req)["status"], "unknown");
            assert_eq!(rpc.calls.len(), 1);
        }
    }

    #[test]
    fn transport_failure_never_retries() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rpc = StubRpc::new(Outcome::Failure("disconnected".to_owned()));
        let req = request(tmp.path());
        let err = prepare_disposable_session(&mut rpc, &req).unwrap_err();
        assert!(err.is_unknown());
        assert!(err.to_string().contains("outcome unknown"));
        assert_eq!(rpc.calls.len(), 1);
        let err = prepare_disposable_session(&mut rpc, &req).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(rpc.calls.len(), 1);
    }

    #[test]
    fn successful_creation_never_repeated() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rpc = StubRpc::new(Outcome::Response(success()));
        let req = request(tmp.path());
        prepare_disposable_session(&mut rpc, &req).unwrap();
        let err = prepare_disposable_session(&mut rpc, &req).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(rpc.calls.len(), 1);
    }

    #[test]
    fn invalid_identity_does_not_contact_gateway() {
        for request_id in [String::new(), " ".to_owned(), "x".repeat(257)] {
            let tmp = tempfile::tempdir().unwrap();
            let mut rpc = StubRpc::new(Outcome::Response(success()));
            let req = SessionCreationRequest {
                request_id,
                record_path: tmp.path().join("creation.json"),
            };
            let err = prepare_disposable_session(&mut rpc, &req).unwrap_err();
            assert!(err.to_string().contains("request_id"));
            assert!(rpc.calls.is_empty());
            assert!(!req.record_path.exists());
        }
    }

    #[test]
    fn existing_creation_barrier_is_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let req = request(tmp.path());
        std::fs::write(&req.record_path, "not even valid json").unwrap();
        let mut rpc = StubRpc::new(Outcome::Response(success()));
        let err = prepare_disposable_session(&mut rpc, &req).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(&req.record_path).unwrap(),
            "not even valid json"
        );
        assert!(rpc.calls.is_empty());
    }
}
