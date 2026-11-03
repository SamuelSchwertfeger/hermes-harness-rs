//! Single-submit live admission probe with durable fail-closed identity tracking.

use crate::util::PyStr as _;
use serde_json::{Map, Value, json};

use super::admission_store::{AdmissionStore, AdmissionStoreError, LocalState};
use super::gateway_identity::{GatewayIdentityError, Receipt, Submission, parse_receipt};
use super::gateway_lifecycle::PROMPT_TEXT_PARAM;
use super::gateway_session_creation::{RpcFailure, SessionRpc};

/// Failure raised by the live admission probe.
#[derive(Debug, thiserror::Error)]
pub enum LiveAdmissionError {
    /// A request argument was invalid (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// The verified receipt was not durably recorded (Python `RuntimeError`).
    #[error("{0}")]
    Runtime(String),
    /// The durable ledger failed.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// Identity contract violation or unknown submit outcome.
    #[error(transparent)]
    Identity(#[from] GatewayIdentityError),
    /// The RPC failed outside the submit call; propagated unchanged.
    #[error("{0}")]
    Rpc(RpcFailure),
}

/// One immutable request body and client-generated submission identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAdmissionRequest {
    /// Submission identity.
    pub submission: Submission,
    /// Probe prompt text.
    pub text: String,
}

/// Receipt identities verified during one live admission probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAdmissionResult {
    /// Gateway admission identity.
    pub admission_id: String,
    /// Status returned by submit.
    pub submit_status: String,
    /// Status returned by the follow-up receipt read.
    pub observed_status: String,
}

///
/// # Errors
/// The request lacks authorization, valid identity, or bounded nonblank text.
fn validate_request(request: &LiveAdmissionRequest) -> Result<(), LiveAdmissionError> {
    if request.text.py_strip().is_empty() {
        return Err(LiveAdmissionError::Value(
            "Probe text must be a nonblank string".to_owned(),
        ));
    }
    if request.submission.admission_id.is_some() {
        return Err(LiveAdmissionError::Value(
            "Live probe requires a submission without an admission ID".to_owned(),
        ));
    }
    Ok(())
}

fn params(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Submit a live admission through the supplied RPC and parse its receipt.
///
/// # Errors
/// The request or gateway receipt is invalid, or ledger or RPC access fails.
pub fn submit_and_parse(
    rpc: &mut dyn SessionRpc,
    request: &LiveAdmissionRequest,
) -> Result<Receipt, LiveAdmissionError> {
    let submit_params = params(json!({
        "session_id": request.submission.session_id,
        "submission_id": request.submission.submission_id,
        (PROMPT_TEXT_PARAM): request.text,
    }));
    let raw_submit = rpc.rpc("prompt.submit", &submit_params).map_err(|error| {
        GatewayIdentityError::OutcomeUnknown {
            message: "prompt.submit outcome unknown; do not automatically retry".to_owned(),
            source: Some(error),
        }
    })?;
    Ok(
        parse_receipt(&raw_submit, None).map_err(|error| GatewayIdentityError::OutcomeUnknown {
            message: "prompt.submit returned no verifiable admission identity; do not retry"
                .to_owned(),
            source: Some(Box::new(error)),
        })?,
    )
}

/// Read a receipt through the supplied RPC and require it to match the admission.
///
/// # Errors
/// The request or gateway receipt is invalid, or ledger or RPC access fails.
pub fn read_matching_receipt(
    rpc: &mut dyn SessionRpc,
    request: &LiveAdmissionRequest,
    accepted: &Receipt,
) -> Result<Receipt, LiveAdmissionError> {
    let receipt_params = params(json!({
        "session_id": request.submission.session_id,
        "admission_id": accepted.admission_id,
    }));
    let raw_observed = rpc
        .rpc("prompt.receipt", &receipt_params)
        .map_err(LiveAdmissionError::Rpc)?;
    Ok(parse_receipt(&raw_observed, Some(&accepted.admission_id))?)
}

/// Submit once, persist the first identity, and verify it with one receipt read.
///
/// The durable record is created before the only submit RPC. A transport failure
/// or malformed submit response leaves the outcome unknown and is never retried.
/// Once the first receipt is valid, its admission ID is persisted before the
/// follow-up read, so a failed receipt read cannot erase known identity.
///
/// # Errors
/// The request or gateway receipt is invalid, or ledger or RPC access fails.
pub fn submit_once(
    store: &AdmissionStore,
    request: &LiveAdmissionRequest,
    rpc: &mut dyn SessionRpc,
) -> Result<LiveAdmissionResult, LiveAdmissionError> {
    validate_request(request)?;
    let submission = &request.submission;
    store.prepare(submission)?;
    store.mark_outcome_unknown(&submission.submission_id)?;

    let accepted = submit_and_parse(rpc, request)?;
    store.record_receipt(&submission.submission_id, &accepted, false)?;
    let observed = read_matching_receipt(rpc, request, &accepted)?;
    store.record_receipt(&submission.submission_id, &observed, false)?;

    let admitted = store
        .get(&submission.submission_id)?
        .is_some_and(|record| record.local_state == LocalState::Admitted);
    if !admitted {
        return Err(LiveAdmissionError::Runtime(
            "Verified receipt was not durably recorded".to_owned(),
        ));
    }

    Ok(LiveAdmissionResult {
        admission_id: accepted.admission_id,
        submit_status: accepted.status,
        observed_status: observed.status,
    })
}
