//! Compose one disposable-session creation with one admission on the same RPC.
//!
//! This module never discovers/starts Hermes. Its caller must explicitly supply an
//! existing authenticated connection; it does not retry either operation.

use super::admission_store::{AdmissionStore, AdmissionStoreError};
use super::gateway_identity::Submission;
use super::gateway_live_admission::{
    LiveAdmissionError, LiveAdmissionRequest, LiveAdmissionResult, submit_once,
};
use super::gateway_session_creation::{
    PreparedSession, SessionCreationError, SessionCreationRequest, SessionRpc,
    prepare_disposable_session,
};
use crate::util::PyStr as _;

/// Failure raised while creating a session and submitting once.
#[derive(Debug, thiserror::Error)]
pub enum DisposableAdmissionError {
    /// A request argument was invalid or already used (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// The durable ledger failed.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// Session creation failed.
    #[error(transparent)]
    Session(#[from] SessionCreationError),
    /// The admission probe failed.
    #[error(transparent)]
    Admission(#[from] LiveAdmissionError),
}

/// Operator-chosen identities plus an immutable harmless prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableAdmissionRequest {
    /// Session creation request.
    pub creation: SessionCreationRequest,
    /// Submission identity.
    pub submission_id: String,
    /// Probe prompt text.
    pub text: String,
}

/// Identities verified on the same gateway connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposableAdmissionResult {
    /// The attached session.
    pub prepared: PreparedSession,
    /// The verified admission.
    pub admission: LiveAdmissionResult,
}

/// Reject invalid or already-used identities before any creation RPC.
///
/// # Errors
/// Disposable-session authorization, profile, or invocation parameters are invalid.
fn validate(
    request: &DisposableAdmissionRequest,
    store: &AdmissionStore,
) -> Result<(), DisposableAdmissionError> {
    if request.submission_id.py_strip().is_empty() {
        return Err(DisposableAdmissionError::Value(
            "submission_id must be nonblank".to_owned(),
        ));
    }
    if request.text.py_strip().is_empty() {
        return Err(DisposableAdmissionError::Value(
            "Probe text must be nonblank".to_owned(),
        ));
    }
    if store.get(&request.submission_id)?.is_some() {
        return Err(DisposableAdmissionError::Value(
            "Submission identity already exists; do not create or submit again".to_owned(),
        ));
    }
    Ok(())
}

/// Create, verify attachment, then submit exactly once using `rpc`.
///
/// The session-creation file is an exclusive no-retry barrier. After a successful
/// creation, the admission ledger is the durable no-retry barrier for submission.
/// No cleanup RPC or compensating cancellation is attempted on failure.
///
/// # Errors
/// Disposable-session validation, session creation, submission, or ledger access fails.
pub fn create_and_submit_once(
    rpc: &mut dyn SessionRpc,
    store: &AdmissionStore,
    request: &DisposableAdmissionRequest,
) -> Result<DisposableAdmissionResult, DisposableAdmissionError> {
    validate(request, store)?;
    let prepared = prepare_disposable_session(rpc, &request.creation)?;
    let admission_request = LiveAdmissionRequest {
        submission: Submission::new(&prepared.session_id, &request.submission_id),
        text: request.text.clone(),
    };
    let admission = submit_once(store, &admission_request, rpc)?;
    Ok(DisposableAdmissionResult {
        prepared,
        admission,
    })
}
