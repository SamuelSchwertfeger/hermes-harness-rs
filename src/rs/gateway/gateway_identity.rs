//! Fail-closed Hermes admission identity and receipt validation (offline only).
//!
//! This module never connects to the gateway. The caller supplies a scripted RPC
//! implementation; authenticated live transport is a separate, gated milestone.

use crate::util::PyStr as _;
use std::collections::BTreeMap;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::uuid4_hex;

/// Errors raised by the identity contract.
#[derive(Debug, thiserror::Error)]
pub enum GatewayIdentityError {
    /// Remote response violates the verified identity contract.
    #[error("{0}")]
    Contract(String),
    /// Transport failed; a request may still have reached its owner.
    #[error("{message}")]
    OutcomeUnknown {
        /// Unchanged outcome description.
        message: String,
        /// Underlying transport failure, when available.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    /// Random identity generation failed.
    #[error("{message}")]
    RandomIdentity {
        /// Unchanged generation failure description.
        message: String,
        /// Random source failure.
        #[source]
        source: io::Error,
    },
}

/// Minimal injectable synchronous RPC seam for offline contract tests.
pub trait Rpc {
    /// Return an untrusted decoded RPC result; `Err` models an `OSError`.
    fn call(&self, method: &str, params: &BTreeMap<String, String>) -> Result<Value, io::Error>;
}

/// Identity and opaque status returned by Hermes (not termination evidence).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// Gateway admission identity.
    pub admission_id: String,
    /// Opaque gateway status.
    pub status: String,
    /// Optional execution generation.
    pub execution_generation: Option<u64>,
}

/// Client-generated request identity, distinct from the gateway admission ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Submission {
    /// Session identity.
    pub session_id: String,
    /// Stable client submission identity.
    pub submission_id: String,
    /// Gateway admission identity once accepted.
    pub admission_id: Option<String>,
}

impl Submission {
    /// Build a submission that has no gateway admission identity yet.
    #[must_use]
    pub fn new(session_id: &str, submission_id: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            submission_id: submission_id.to_owned(),
            admission_id: None,
        }
    }

    /// Build a submission that already carries a gateway admission identity.
    #[must_use]
    pub fn admitted(session_id: &str, submission_id: &str, admission_id: &str) -> Self {
        Self {
            admission_id: Some(admission_id.to_owned()),
            ..Self::new(session_id, submission_id)
        }
    }
}

fn contract(msg: impl Into<String>) -> GatewayIdentityError {
    GatewayIdentityError::Contract(msg.into())
}

/// Require a nonblank string.
///
/// # Errors
/// The value is absent, empty, or contains only whitespace.
pub fn nonblank_str<'a>(
    value: Option<&'a str>,
    key: &str,
) -> Result<&'a str, GatewayIdentityError> {
    match value {
        Some(text) if !text.py_strip().is_empty() => Ok(text),
        _ => Err(contract(format!("Gateway {key} must be a nonblank string"))),
    }
}

/// Require a JSON value that is a nonblank string.
///
/// # Errors
/// The value is absent, is not a string, or contains only whitespace.
pub fn nonblank<'a>(value: Option<&'a Value>, key: &str) -> Result<&'a str, GatewayIdentityError> {
    nonblank_str(value.and_then(Value::as_str), key)
}

/// Validate identity without speculating about Hermes terminal status values.
///
/// # Errors
/// The receipt shape, admission identity, status, or execution generation is invalid.
pub fn parse_receipt(
    raw: &Value,
    expected_admission_id: Option<&str>,
) -> Result<Receipt, GatewayIdentityError> {
    let Some(object) = raw.as_object() else {
        return Err(contract("Gateway receipt must be a JSON object"));
    };
    let admission_id = nonblank(object.get("admission_id"), "admission_id")?;
    let status = nonblank(object.get("status"), "status")?;
    if expected_admission_id.is_some_and(|expected| admission_id != expected) {
        return Err(contract("Gateway receipt changed admission identity"));
    }
    let bad_generation = || contract("Gateway execution_generation must be a nonnegative integer");
    let generation = match object.get("execution_generation") {
        None | Some(Value::Null) => None,
        Some(Value::Number(number)) => Some(number.as_u64().ok_or_else(bad_generation)?),
        Some(_) => return Err(bad_generation()),
    };
    Ok(Receipt {
        admission_id: admission_id.to_owned(),
        status: status.to_owned(),
        execution_generation: generation,
    })
}

/// Validate scripted Hermes calls; never retries on communication failure.
pub struct OfflineAdmissionClient<'a, R: Rpc + ?Sized> {
    rpc: &'a R,
}

impl<'a, R: Rpc + ?Sized> OfflineAdmissionClient<'a, R> {
    /// Wrap an injected RPC implementation.
    #[must_use]
    pub const fn new(rpc: &'a R) -> Self {
        Self { rpc }
    }

    /// Create a submission with a fresh random `submission_id` (Python `new`).
    ///
    /// # Errors
    /// The session identity is blank or random ID generation fails.
    pub fn new_submission(session_id: &str) -> Result<Submission, GatewayIdentityError> {
        let session = nonblank_str(Some(session_id), "session_id")?;
        let id = uuid4_hex().map_err(|error| GatewayIdentityError::RandomIdentity {
            message: format!("Cannot generate submission_id: {error}"),
            source: error,
        })?;
        Ok(Submission::new(session, &id))
    }

    fn call(
        &self,
        method: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<Value, GatewayIdentityError> {
        self.rpc
            .call(method, params)
            .map_err(|error| GatewayIdentityError::OutcomeUnknown {
                message: format!("{method} outcome unknown; do not automatically retry"),
                source: Some(Box::new(error)),
            })
    }

    /// Send once, using Hermes's `text` field and stable `submission_id`.
    ///
    /// # Errors
    /// The submission or receipt is invalid. Transport failure does not establish whether
    /// submission occurred.
    pub fn submit(
        &self,
        submission: &Submission,
        text: &str,
    ) -> Result<(Submission, Receipt), GatewayIdentityError> {
        if submission.admission_id.is_some() {
            return Err(contract(
                "Submission already has a gateway admission identity",
            ));
        }
        let params = BTreeMap::from([
            ("session_id".to_owned(), submission.session_id.clone()),
            ("submission_id".to_owned(), submission.submission_id.clone()),
            (
                "text".to_owned(),
                nonblank_str(Some(text), "text")?.to_owned(),
            ),
        ]);
        let response = self.call("prompt.submit", &params)?;
        let receipt = parse_receipt(&response, None)?;
        let accepted = Submission {
            session_id: submission.session_id.clone(),
            submission_id: submission.submission_id.clone(),
            admission_id: Some(receipt.admission_id.clone()),
        };
        Ok((accepted, receipt))
    }

    fn admission_call(
        &self,
        method: &str,
        submission: &Submission,
    ) -> Result<Receipt, GatewayIdentityError> {
        let admission_id = nonblank_str(submission.admission_id.as_deref(), "admission_id")?;
        let params = BTreeMap::from([
            ("session_id".to_owned(), submission.session_id.clone()),
            ("admission_id".to_owned(), admission_id.to_owned()),
        ]);
        parse_receipt(&self.call(method, &params)?, Some(admission_id))
    }

    /// Fetch the receipt for an admitted submission.
    ///
    /// # Errors
    /// The identity or receipt is invalid, or the injected RPC fails.
    pub fn receipt(&self, submission: &Submission) -> Result<Receipt, GatewayIdentityError> {
        self.admission_call("prompt.receipt", submission)
    }

    /// Request queued cancellation; response alone does not prove execution stopped.
    ///
    /// # Errors
    /// The identity or receipt is invalid, or the injected RPC fails.
    pub fn cancel_queued(&self, submission: &Submission) -> Result<Receipt, GatewayIdentityError> {
        self.admission_call("prompt.cancel", submission)
    }
}

impl<R: Rpc + ?Sized> std::fmt::Debug for OfflineAdmissionClient<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfflineAdmissionClient")
            .field("injected_dependencies", &true)
            .finish_non_exhaustive()
    }
}
