//! Fail-closed recovery of uncertain admissions using existing read-only RPCs.
//!
//! The injected transports must already be authenticated. This module never submits,
//! starts a gateway, retries a request, or treats pending-list absence as rejection.

use super::admission_store::{AdmissionRecord, AdmissionStore, AdmissionStoreError, LocalState};
use super::gateway_event_replay::read_pending_snapshot;
use super::gateway_identity::Rpc;
use super::gateway_pending::{PendingDisposition, inspect_pending};
use super::gateway_readonly::{GatewayReadError, GatewayRpc};
use super::gateway_resolver::KnownAdmissionReceiptResolver;
use crate::core::reconciliation::{
    LookupResult, ReconciliationError, ReconciliationStatus, ResolveError, SubmissionResolver,
    reconcile_unknown,
};

/// Failure while recovering an unknown admission.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    /// The durable ledger failed.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// Reconciliation failed; the record is unchanged.
    #[error(transparent)]
    Reconciliation(#[from] ReconciliationError),
    /// The read-only replay failed.
    #[error(transparent)]
    Read(#[from] GatewayReadError),
}

/// Already-authenticated, injected read-only gateway clients.
pub struct RecoveryClients<'a, R: GatewayRpc + ?Sized, C: Rpc + ?Sized> {
    /// Event replay connection.
    pub read_rpc: &'a mut R,
    /// Receipt lookup connection.
    pub receipt_rpc: &'a C,
}

/// Bridge a snapshot candidate to the existing validated receipt resolver.
struct CandidateReceiptResolver<'a, C: Rpc + ?Sized> {
    resolver: KnownAdmissionReceiptResolver<'a, C>,
    candidate_id: String,
}

impl<C: Rpc + ?Sized> SubmissionResolver for CandidateReceiptResolver<'_, C> {
    /// Do exactly one receipt lookup with the candidate, never a submit.
    fn resolve(&self, record: &AdmissionRecord) -> Result<LookupResult, ResolveError> {
        let candidate = AdmissionRecord {
            admission_id: Some(self.candidate_id.clone()),
            ..record.clone()
        };
        self.resolver.resolve(&candidate)
    }
}

/// Reconcile an unknown record without ever redispatching its submission.
///
/// Known admission IDs need only a receipt lookup. Otherwise inspect a complete,
/// epoch-bound `session.events.since` replay for a *candidate* admission ID,
/// then validate the candidate with `prompt.receipt` before mutating the ledger.
/// Any uncertainty leaves the durable record unchanged.
///
/// # Errors
/// The replay or receipt cannot verify the admission, or RPC or ledger access fails.
pub fn recover_unknown_admission<R: GatewayRpc + ?Sized, C: Rpc + ?Sized>(
    store: &AdmissionStore,
    submission_id: &str,
    clients: &mut RecoveryClients<'_, R, C>,
) -> Result<ReconciliationStatus, RecoveryError> {
    let Some(record) = store.get(submission_id)? else {
        return Ok(ReconciliationStatus::NotEligible);
    };
    if record.local_state != LocalState::OutcomeUnknown {
        return Ok(ReconciliationStatus::NotEligible);
    }

    let receipt_rpc = clients.receipt_rpc;
    if record.admission_id.is_some() {
        let resolver = KnownAdmissionReceiptResolver::new(receipt_rpc);
        return Ok(reconcile_unknown(store, submission_id, &resolver)?);
    }

    let Some(snapshot) = read_pending_snapshot(&mut *clients.read_rpc, &record.session_id)? else {
        return Ok(ReconciliationStatus::StillUnknown);
    };
    let candidate = inspect_pending(&snapshot, &record.session_id, &record.submission_id);
    let (PendingDisposition::Found, Some(candidate_id)) =
        (candidate.disposition, candidate.admission_id)
    else {
        return Ok(ReconciliationStatus::StillUnknown);
    };

    let resolver = CandidateReceiptResolver {
        resolver: KnownAdmissionReceiptResolver::new(receipt_rpc),
        candidate_id,
    };
    Ok(reconcile_unknown(store, submission_id, &resolver)?)
}

impl<R: GatewayRpc + ?Sized, C: Rpc + ?Sized> std::fmt::Debug for RecoveryClients<'_, R, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryClients")
            .field("injected_dependencies", &true)
            .finish_non_exhaustive()
    }
}
