//! Fail-closed reconciliation for submissions with uncertain gateway outcomes.
//!
//! This module does not implement a Hermes RPC method. A caller must inject a resolver
//! whose lookup-by-submission-ID semantics have been verified against the installed
//! Hermes gateway. Missing or ambiguous results never authorize resubmission.

use std::fmt;

use crate::gateway::admission_store::{
    AdmissionRecord, AdmissionStore, AdmissionStoreError, LocalState,
};
use crate::gateway::gateway_identity::Receipt;

/// Error type an injected resolver may fail with (Python: any exception).
pub type ResolveError = Box<dyn std::error::Error + Send + Sync>;

/// A resolver returned a result that violates the reconciliation contract.
#[derive(Debug, thiserror::Error)]
pub enum ReconciliationError {
    /// The resolver result violates the contract.
    #[error("{0}")]
    Reconciliation(String),
    /// A caller argument was invalid (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// The durable ledger failed.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// The injected resolver failed; the durable record is unchanged.
    #[error("{0}")]
    Resolver(ResolveError),
}

/// Meaning of a resolver's authoritative lookup result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LookupDisposition {
    /// The request was found.
    Found,
    /// Not found; not proof of non-admission.
    NotFound,
    /// Result is ambiguous.
    Indeterminate,
}

impl LookupDisposition {
    /// The string value Python's `StrEnum` uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Found => "found",
            Self::NotFound => "not_found",
            Self::Indeterminate => "indeterminate",
        }
    }
}

impl fmt::Display for LookupDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Harness result; deliberately not a Hermes execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReconciliationStatus {
    /// A verified receipt was persisted.
    ReceiptRecorded,
    /// The record stays unknown.
    StillUnknown,
    /// The record is not eligible for reconciliation.
    NotEligible,
}

impl ReconciliationStatus {
    /// The string value Python's `StrEnum` uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReceiptRecorded => "receipt_recorded",
            Self::StillUnknown => "still_unknown",
            Self::NotEligible => "not_eligible",
        }
    }
}

impl fmt::Display for ReconciliationStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of a verified lookup by stable submission identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupResult {
    disposition: LookupDisposition,
    receipt: Option<Receipt>,
}

impl LookupResult {
    /// Build a result, requiring a receipt exactly when the disposition is `Found`.
    ///
    /// # Errors
    /// The admission identity or receipt is invalid, or ledger or resolver access fails.
    pub fn new(
        disposition: LookupDisposition,
        receipt: Option<Receipt>,
    ) -> Result<Self, ReconciliationError> {
        if (disposition == LookupDisposition::Found) != receipt.is_some() {
            return Err(ReconciliationError::Value(
                "A receipt is required only when disposition is FOUND".to_owned(),
            ));
        }
        Ok(Self {
            disposition,
            receipt,
        })
    }

    /// What the lookup established.
    #[must_use]
    pub const fn disposition(&self) -> LookupDisposition {
        self.disposition
    }

    /// Receipt, present only when found.
    #[must_use]
    pub const fn receipt(&self) -> Option<&Receipt> {
        self.receipt.as_ref()
    }
}

/// Injected lookup adapter; it must not submit, retry, or start Hermes.
pub trait SubmissionResolver {
    /// Look up the existing request by session and stable submission ID.
    fn resolve(&self, record: &AdmissionRecord) -> Result<LookupResult, ResolveError>;
}

/// Reconcile an uncertain request without retrying or guessing.
///
/// Only records in `OutcomeUnknown` are eligible. A verified receipt is persisted
/// with `reconciled = true`. `NotFound` and `Indeterminate` leave the record unknown:
/// absence from a lookup is not proof that the original request was never admitted.
/// Resolver failures propagate while the durable record remains unchanged.
///
/// # Errors
/// The admission identity or receipt is invalid, or ledger or resolver access fails.
pub fn reconcile_unknown<R: SubmissionResolver + ?Sized>(
    store: &AdmissionStore,
    submission_id: &str,
    resolver: &R,
) -> Result<ReconciliationStatus, ReconciliationError> {
    let Some(record) = store.get(submission_id)? else {
        return Ok(ReconciliationStatus::NotEligible);
    };
    if record.local_state != LocalState::OutcomeUnknown {
        return Ok(ReconciliationStatus::NotEligible);
    }

    let result = resolver
        .resolve(&record)
        .map_err(ReconciliationError::Resolver)?;
    if result.disposition() != LookupDisposition::Found {
        return Ok(ReconciliationStatus::StillUnknown);
    }

    let Some(receipt) = result.receipt() else {
        return Err(ReconciliationError::Reconciliation(
            "FOUND lookup result is missing its receipt".to_owned(),
        ));
    };
    if record
        .admission_id
        .as_deref()
        .is_some_and(|known| known != receipt.admission_id)
    {
        return Err(ReconciliationError::Reconciliation(
            "Resolved receipt conflicts with the durable admission ID".to_owned(),
        ));
    }

    store.record_receipt(submission_id, receipt, true)?;
    Ok(ReconciliationStatus::ReceiptRecorded)
}
