//! Read-only terminal receipt reconciliation for a previously admitted prompt.
//!
//! No RPC in this module can create a session, submit a prompt, cancel a turn,
//! or start a gateway. The owner remains the authority for execution outcomes.

use std::collections::BTreeMap;

use serde_json::Value;

use super::gateway_identity::{GatewayIdentityError, Receipt, parse_receipt};
pub use super::gateway_readonly::GatewayRpc as ReceiptReader;
use super::gateway_readonly::GatewayRpcError;

/// Boxed failure of an admission ledger implementation.
pub type LedgerError = Box<dyn std::error::Error + Send + Sync>;

/// Errors raised while reconciling a terminal receipt.
#[derive(Debug, thiserror::Error)]
pub enum TerminalReconciliationError {
    /// The observed receipt does not prove a terminal result.
    #[error("{0}")]
    Reconciliation(String),
    /// The receipt violates the identity contract.
    #[error(transparent)]
    Contract(#[from] GatewayIdentityError),
    /// The receipt RPC failed; propagated without retry.
    #[error(transparent)]
    Rpc(#[from] GatewayRpcError),
    /// The admission ledger failed.
    #[error("{0}")]
    Ledger(LedgerError),
}

/// Identity fields of a persisted admission that reconciliation needs.
pub trait AdmissionRecordView {
    /// Gateway session identity.
    fn session_id(&self) -> &str;
    /// Gateway admission identity once known.
    fn admission_id(&self) -> Option<&str>;
}

/// Durable admission ledger seam (Python `AdmissionStore`).
pub trait AdmissionLedger {
    /// Persisted record type.
    type Record: AdmissionRecordView + Clone;
    /// Ledger failure type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Return one record without changing its state.
    fn get(&self, submission_id: &str) -> Result<Option<Self::Record>, Self::Error>;

    /// Persist a validated receipt; `reconciled` marks an authoritative lookup.
    fn record_receipt(
        &self,
        submission_id: &str,
        receipt: &Receipt,
        reconciled: bool,
    ) -> Result<Self::Record, Self::Error>;
}

/// Sanitized terminal execution evidence; never stores a model response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEvidence<R> {
    /// Client submission identity.
    pub submission_id: String,
    /// Gateway admission identity.
    pub admission_id: String,
    /// Verified terminal outcome.
    pub outcome: String,
    /// Observed execution generation.
    pub execution_generation: Option<u64>,
    /// Final response text, if any.
    pub final_response: Option<String>,
    /// Ledger record after persisting the receipt.
    pub recorded: R,
}

fn fail<T>(msg: &str) -> Result<T, TerminalReconciliationError> {
    Err(TerminalReconciliationError::Reconciliation(msg.to_owned()))
}

fn flag(result: &Value, key: &str) -> bool {
    result.get(key) == Some(&Value::Bool(true))
}

/// Check the outcome against the result flags and extract the final response.
///
/// # Errors
/// The terminal outcome or result does not satisfy the evidence contract.
pub fn verify_outcome_result(
    outcome: &str,
    result: Option<&Value>,
) -> Result<Option<String>, TerminalReconciliationError> {
    let object = result.filter(|value| value.is_object());
    if outcome == "completed" {
        let Some(result) = object.filter(|value| flag(value, "completed")) else {
            return fail("Completed outcome lacks committed completion evidence");
        };
        if flag(result, "failed") || flag(result, "interrupted") {
            return fail("Completed outcome conflicts with execution flags");
        }
    } else if object.is_some_and(|value| flag(value, "completed")) {
        return fail("Non-completed outcome conflicts with completion flag");
    }
    match object.and_then(|value| value.get("final_response")) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => fail("Final response must be text or null"),
    }
}

/// Verify a matching terminal receipt and return its outcome and final response.
///
/// # Errors
/// The receipt identity, execution generation, or terminal outcome evidence is invalid.
pub fn verify_terminal(
    raw: &Value,
    expected_id: &str,
) -> Result<(String, Option<String>), TerminalReconciliationError> {
    let Some(object) = raw.as_object() else {
        return Err(GatewayIdentityError::Contract("Receipt must be an object".to_owned()).into());
    };
    let admission_matches = object.get("admission_id").and_then(Value::as_str) == Some(expected_id);
    if !admission_matches || object.get("status").and_then(Value::as_str) != Some("terminal") {
        return fail("Matching terminal receipt is required");
    }
    let outcome = object
        .get("outcome")
        .and_then(Value::as_str)
        .filter(|name| ["completed", "failed", "interrupted", "cancelled"].contains(name));
    let Some(outcome) = outcome else {
        return fail("Terminal outcome must be explicit and recognized");
    };
    let response = verify_outcome_result(outcome, object.get("result"))?;
    Ok((outcome.to_owned(), response))
}

/// Read one authoritative receipt, then persist only verified terminal state.
///
/// Never substitutes a submission lookup or retries an uncertain RPC. Missing
/// admission identity is an explicit blocker, not permission to resubmit.
///
/// # Errors
/// Terminal evidence or admission identity is invalid, or receipt or ledger access fails.
pub fn reconcile_terminal_once<R, S>(
    rpc: &mut R,
    store: &S,
    submission_id: &str,
) -> Result<TerminalEvidence<S::Record>, TerminalReconciliationError>
where
    R: ReceiptReader + ?Sized,
    S: AdmissionLedger + ?Sized,
{
    let record = store
        .get(submission_id)
        .map_err(|error| TerminalReconciliationError::Ledger(Box::new(error)))?;
    let Some((record, admission_id)) = record.and_then(|record| {
        let admission_id = record.admission_id().map(str::to_owned)?;
        Some((record, admission_id))
    }) else {
        return fail("Known durable admission ID is required; never resubmit");
    };
    let params = BTreeMap::from([
        ("session_id".to_owned(), Value::from(record.session_id())),
        (
            "admission_id".to_owned(),
            Value::from(admission_id.as_str()),
        ),
        ("include_result".to_owned(), Value::Bool(true)),
    ]);
    let raw = rpc.rpc("prompt.receipt", &params)?;
    let receipt = parse_receipt(&raw, Some(&admission_id))?;
    let reference_session = raw.get("ref").filter(|value| value.is_object());
    if reference_session.is_some_and(|reference| {
        reference.get("session_id").and_then(Value::as_str) != Some(record.session_id())
    }) {
        return fail("Receipt session identity changed");
    }
    let (outcome, final_response) = verify_terminal(&raw, &admission_id)?;
    let recorded = store
        .record_receipt(submission_id, &receipt, true)
        .map_err(|error| TerminalReconciliationError::Ledger(Box::new(error)))?;
    Ok(TerminalEvidence {
        submission_id: submission_id.to_owned(),
        admission_id,
        outcome,
        execution_generation: receipt.execution_generation,
        final_response,
        recorded,
    })
}

impl AdmissionRecordView for super::admission_store::AdmissionRecord {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn admission_id(&self) -> Option<&str> {
        self.admission_id.as_deref()
    }
}

/// The production ledger is the sqlite [`AdmissionStore`](super::admission_store::AdmissionStore).
impl AdmissionLedger for super::admission_store::AdmissionStore {
    type Record = super::admission_store::AdmissionRecord;
    type Error = super::admission_store::AdmissionStoreError;

    fn get(&self, submission_id: &str) -> Result<Option<Self::Record>, Self::Error> {
        Self::get(self, submission_id)
    }

    fn record_receipt(
        &self,
        submission_id: &str,
        receipt: &Receipt,
        reconciled: bool,
    ) -> Result<Self::Record, Self::Error> {
        Self::record_receipt(self, submission_id, receipt, reconciled)
    }
}
