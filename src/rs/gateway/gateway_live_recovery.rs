//! Compose existing-gateway attachment with safe read-only admission recovery.

use super::admission_store::{AdmissionStore, AdmissionStoreError, LocalState};
use super::gateway_async_recovery::{AsyncRecoveryError, recover_with_async_rpc};
use super::gateway_attachment::{
    GatewayAttachmentConfig, GatewayAttachmentError, GatewayRpc, attach_existing_gateway,
};
use crate::core::reconciliation::ReconciliationStatus;

/// Failure while recovering through an existing gateway.
#[derive(Debug, thiserror::Error)]
pub enum LiveRecoveryError {
    /// The durable ledger failed.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// Attachment to the existing gateway failed.
    #[error(transparent)]
    Attachment(#[from] GatewayAttachmentError),
    /// Recovery over the attached connection failed.
    #[error(transparent)]
    Recovery(#[from] AsyncRecoveryError),
}

/// Recover through an existing authenticated gateway, without starting one.
///
/// The durable record is checked before discovery so ineligible work never opens
/// a connection. Discovery must be read-only, and the connector receives the exact
/// discovered endpoint so it cannot silently start or replace a gateway.
///
/// # Errors
/// Gateway qualification, replay validation, receipt lookup, or ledger access fails.
pub fn recover_with_existing_gateway<E, R: GatewayRpc>(
    store: &AdmissionStore,
    submission_id: &str,
    config: &GatewayAttachmentConfig<'_, E, R>,
) -> Result<ReconciliationStatus, LiveRecoveryError> {
    let Some(record) = store.get(submission_id)? else {
        return Ok(ReconciliationStatus::NotEligible);
    };
    if record.local_state != LocalState::OutcomeUnknown {
        return Ok(ReconciliationStatus::NotEligible);
    }

    Ok(attach_existing_gateway(config, |rpc| {
        recover_with_async_rpc(store, submission_id, rpc)
    })??)
}
