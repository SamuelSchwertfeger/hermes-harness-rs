//! Fail-closed, simulation-only Hermes admission lifecycle tracking.
//!
//! The real gateway RPC schema has not been verified. No production connection is
//! constructed here; an injected RPC seam is accepted only with explicit opt-in.

use crate::util::PyStr as _;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::util::uuid4_hex;

/// Gateway prompt.submit parameter carrying the prompt body.
pub(crate) const PROMPT_TEXT_PARAM: &str = "text";

crate::util::wire_enum! {
    /// Lifecycle state of one admission.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum AdmissionState {
        /// Created locally, nothing sent.
        New => "new",
        /// Submission attempted with an unknown outcome.
        SubmitUnknown => "submit_unknown",
        /// A receipt response was recorded.
        ReceiptRecorded => "receipt_recorded",
        /// Cancellation was accepted as a request.
        CancelRequested => "cancel_requested",
        /// Cancellation was attempted with an unknown outcome.
        CancelUnknown => "cancel_unknown",
        /// Terminal state that is not verified.
        TerminalUnverified => "terminal_unverified",
    }

    /// The string value Python's `StrEnum` uses.
    as_str;
}

/// Reject untrusted lifecycle operations or inconsistent state.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    /// The operation is not allowed.
    #[error("{0}")]
    Rejected(String),
    /// Evidence could not be written.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Evidence could not be serialised.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

fn rejected(message: &str) -> AdmissionError {
    AdmissionError::Rejected(message.to_owned())
}

/// Injected RPC transport; use a scripted fake until the API is verified.
///
/// An `Err` models `OSError` (a disconnect); any `Ok` value is a parsed response.
pub trait GatewayRpc {
    /// Invoke a scripted method and return a parsed response.
    fn call(&mut self, method: &str, params: &BTreeMap<String, String>) -> io::Result<Value>;
}

/// One tracked admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admission {
    /// Stable session identifier.
    pub session_id: String,
    /// Unique submission identifier.
    pub submission_id: String,
    /// Current state.
    pub state: AdmissionState,
    /// Last recorded RPC response, if any.
    pub receipt: Option<Map<String, Value>>,
}

impl Admission {
    fn with_state(&self, state: AdmissionState, receipt: Option<Map<String, Value>>) -> Self {
        Self {
            session_id: self.session_id.clone(),
            submission_id: self.submission_id.clone(),
            state,
            receipt,
        }
    }
}

/// Lifecycle configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleConfig {
    /// Directory for private evidence files.
    pub evidence_root: PathBuf,
    /// Must stay true: live transport is not verified.
    pub simulated_only: bool,
}

impl LifecycleConfig {
    /// Simulation-only configuration rooted at `evidence_root`.
    #[must_use]
    pub fn new(evidence_root: impl Into<PathBuf>) -> Self {
        Self {
            evidence_root: evidence_root.into(),
            simulated_only: true,
        }
    }
}

#[cfg(unix)]
fn ensure_lifecycle_dir(root: &Path) -> io::Result<()> {
    if root.is_dir() {
        return Ok(());
    }
    if let Some(parent) = root.parent() {
        fs::create_dir_all(parent)?;
    }
    match crate::util::create_private_dir(root) {
        Err(err) if err.kind() != io::ErrorKind::AlreadyExists => Err(err),
        _ => Ok(()),
    }
}

#[cfg(not(unix))]
fn ensure_lifecycle_dir(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

/// Persist decisions without claiming that remote cancellation stopped work.
pub struct GatewayLifecycle<'a> {
    rpc: &'a mut dyn GatewayRpc,
    config: LifecycleConfig,
}

impl<'a> GatewayLifecycle<'a> {
    /// Build a lifecycle; refuses any non-simulated configuration.
    ///
    /// # Errors
    /// Configuration or admission state is invalid, evidence I/O fails, or the injected RPC fails.
    pub fn new(
        rpc: &'a mut dyn GatewayRpc,
        config: LifecycleConfig,
    ) -> Result<Self, AdmissionError> {
        if !config.simulated_only {
            return Err(rejected(
                "Live gateway transport is not verified or enabled",
            ));
        }
        Ok(Self { rpc, config })
    }

    fn save(&self, admission: Admission) -> Result<Admission, AdmissionError> {
        let root = &self.config.evidence_root;
        ensure_lifecycle_dir(root)?;
        let payload = crate::util::sorted_json_spaced(&admission)?;
        let path = root.join(format!("{}.json", admission.submission_id));
        if path.exists() {
            fs::write(&path, payload.as_bytes())?;
        } else {
            // Exclusive creation prevents silent replacement of an unrelated admission.
            let mut stream = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            io::Write::write_all(&mut stream, payload.as_bytes())?;
        }
        #[cfg(unix)]
        restrict_file(&path)?;
        Ok(admission)
    }

    /// Start a fresh admission for `session_id`.
    ///
    /// # Errors
    /// Configuration or admission state is invalid, evidence I/O fails, or the injected RPC fails.
    pub fn new_admission(&self, session_id: &str) -> Result<Admission, AdmissionError> {
        if session_id.py_strip().is_empty() {
            return Err(rejected("A stable, nonblank session ID is required"));
        }
        self.save(Admission {
            session_id: session_id.to_owned(),
            submission_id: uuid4_hex()?,
            state: AdmissionState::New,
            receipt: None,
        })
    }

    /// Simulate submission; an exception always leaves the outcome unknown.
    ///
    /// # Errors
    /// Configuration or admission state is invalid, evidence I/O fails, or the injected RPC fails.
    pub fn submit(
        &mut self,
        admission: &Admission,
        prompt: &str,
    ) -> Result<Admission, AdmissionError> {
        if admission.state != AdmissionState::New || prompt.py_strip().is_empty() {
            return Err(rejected(
                "Only a fresh admission with a nonblank prompt may be submitted",
            ));
        }
        let uncertain = self.save(admission.with_state(AdmissionState::SubmitUnknown, None))?;
        let params = BTreeMap::from([
            ("session_id".to_owned(), uncertain.session_id.clone()),
            ("submission_id".to_owned(), uncertain.submission_id.clone()),
            (PROMPT_TEXT_PARAM.to_owned(), prompt.to_owned()),
        ]);
        let Ok(response) = self.rpc.call("prompt.submit", &params) else {
            return Ok(uncertain);
        };
        self.record(&uncertain, response)
    }

    fn record(&self, admission: &Admission, response: Value) -> Result<Admission, AdmissionError> {
        let Value::Object(receipt) = response else {
            return Err(rejected("RPC must return a JSON object"));
        };
        // A receipt is not proof of completed execution.
        self.save(admission.with_state(AdmissionState::ReceiptRecorded, Some(receipt)))
    }

    /// Reconcile the same ID; never retry a potentially accepted submission.
    ///
    /// # Errors
    /// Configuration or admission state is invalid, evidence I/O fails, or the injected RPC fails.
    pub fn receipt(&mut self, admission: &Admission) -> Result<Admission, AdmissionError> {
        if !matches!(
            admission.state,
            AdmissionState::SubmitUnknown
                | AdmissionState::ReceiptRecorded
                | AdmissionState::CancelUnknown
                | AdmissionState::CancelRequested
        ) {
            return Err(rejected(
                "Receipt lookup is not valid for this admission state",
            ));
        }
        let params = BTreeMap::from([
            ("session_id".to_owned(), admission.session_id.clone()),
            ("submission_id".to_owned(), admission.submission_id.clone()),
        ]);
        let Ok(response) = self.rpc.call("prompt.receipt", &params) else {
            return Ok(admission.clone());
        };
        self.record(admission, response)
    }

    /// Request cancellation without treating acceptance as termination proof.
    ///
    /// # Errors
    /// Configuration or admission state is invalid, evidence I/O fails, or the injected RPC fails.
    pub fn cancel(
        &mut self,
        admission: &Admission,
        admission_id: &str,
    ) -> Result<Admission, AdmissionError> {
        if admission_id.py_strip().is_empty()
            || !matches!(
                admission.state,
                AdmissionState::ReceiptRecorded | AdmissionState::CancelUnknown
            )
        {
            return Err(rejected(
                "Cancellation requires a known receipt and explicit admission ID",
            ));
        }
        let pending = self
            .save(admission.with_state(AdmissionState::CancelUnknown, admission.receipt.clone()))?;
        let params = BTreeMap::from([
            ("session_id".to_owned(), pending.session_id.clone()),
            ("admission_id".to_owned(), admission_id.to_owned()),
        ]);
        match self.rpc.call("prompt.cancel", &params) {
            Ok(Value::Object(response)) => {
                self.save(pending.with_state(AdmissionState::CancelRequested, Some(response)))
            }
            Ok(_) | Err(_) => Ok(pending),
        }
    }
}

impl fmt::Debug for GatewayLifecycle<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GatewayLifecycle")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}
