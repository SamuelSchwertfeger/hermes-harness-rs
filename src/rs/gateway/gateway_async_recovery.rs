//! Read-only recovery through one already authenticated Hermes RPC connection.
//!
//! The injected RPC must attach to an existing gateway. This module does not import
//! Hermes, discover endpoints, start services, submit prompts, or retry failed RPCs.
//! The Python original is asynchronous; here the injected transport is synchronous.

use std::collections::BTreeMap;

use serde_json::Value;

use super::admission_store::{AdmissionRecord, AdmissionStore, AdmissionStoreError, LocalState};
use super::gateway_event_replay::read_pending_snapshot;
use super::gateway_identity::{GatewayIdentityError, parse_receipt};
use super::gateway_pending::{PendingDisposition, inspect_pending};
use super::gateway_readonly::{GatewayReadError, GatewayRpc, GatewayRpcError};
use crate::core::reconciliation::{
    LookupDisposition, LookupResult, ReconciliationError, ReconciliationStatus, ResolveError,
    SubmissionResolver, reconcile_unknown,
};

/// Failure while recovering through an injected RPC.
#[derive(Debug, thiserror::Error)]
pub enum AsyncRecoveryError {
    /// The durable ledger failed.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// Reconciliation failed; the record is unchanged.
    #[error(transparent)]
    Reconciliation(#[from] ReconciliationError),
    /// The read-only replay failed.
    #[error(transparent)]
    Read(#[from] GatewayReadError),
    /// The receipt RPC failed.
    #[error(transparent)]
    Rpc(#[from] GatewayRpcError),
    /// The receipt violated the identity contract.
    #[error(transparent)]
    Identity(#[from] GatewayIdentityError),
}

/// One-shot adapter for the existing durable reconciliation transaction.
struct VerifiedReceipt {
    result: LookupResult,
}

impl SubmissionResolver for VerifiedReceipt {
    /// Return only the already validated server receipt; perform no I/O.
    fn resolve(&self, _record: &AdmissionRecord) -> Result<LookupResult, ResolveError> {
        Ok(self.result.clone())
    }
}

/// Read a pending candidate, verify its receipt, and persist it if authoritative.
///
/// Existing admission IDs skip event replay. Unknown IDs require exactly one
/// matching pending entry from a complete `session.events.since` replay;
/// absence is never proof of non-admission. Failures leave the ledger unchanged.
///
/// # Errors
/// Gateway attachment, replay validation, receipt validation, or ledger access fails.
pub fn recover_with_async_rpc<R: GatewayRpc + ?Sized>(
    store: &AdmissionStore,
    submission_id: &str,
    rpc: &mut R,
) -> Result<ReconciliationStatus, AsyncRecoveryError> {
    let Some(record) = store.get(submission_id)? else {
        return Ok(ReconciliationStatus::NotEligible);
    };
    if record.local_state != LocalState::OutcomeUnknown {
        return Ok(ReconciliationStatus::NotEligible);
    }

    let admission_id = if let Some(known) = record.admission_id.clone() {
        known
    } else {
        let Some(snapshot) = read_pending_snapshot(rpc, &record.session_id)? else {
            return Ok(ReconciliationStatus::StillUnknown);
        };
        let candidate = inspect_pending(&snapshot, &record.session_id, &record.submission_id);
        let (PendingDisposition::Found, Some(candidate_id)) =
            (candidate.disposition, candidate.admission_id)
        else {
            return Ok(ReconciliationStatus::StillUnknown);
        };
        candidate_id
    };

    let params = BTreeMap::from([
        (
            "session_id".to_owned(),
            Value::from(record.session_id.as_str()),
        ),
        (
            "admission_id".to_owned(),
            Value::from(admission_id.as_str()),
        ),
    ]);
    let raw = rpc.rpc("prompt.receipt", &params)?;
    let receipt = parse_receipt(&raw, Some(&admission_id))?;
    let result = LookupResult::new(LookupDisposition::Found, Some(receipt))?;
    Ok(reconcile_unknown(
        store,
        submission_id,
        &VerifiedReceipt { result },
    )?)
}
