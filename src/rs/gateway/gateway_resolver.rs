//! Receipt reconciliation using only Hermes's known-admission receipt RPC.
//!
//! This adapter cannot recover a lost admission ID. When that ID is unknown, it
//! returns `Indeterminate` without making an RPC call. It never submits or retries work.

use std::collections::BTreeMap;

use super::admission_store::AdmissionRecord;
use super::gateway_identity::{Rpc, parse_receipt};
use crate::core::reconciliation::{
    LookupDisposition, LookupResult, ResolveError, SubmissionResolver,
};

/// Resolve receipt state only when the durable ledger already knows `admission_id`.
pub struct KnownAdmissionReceiptResolver<'a, R: Rpc + ?Sized> {
    rpc: &'a R,
}

impl<'a, R: Rpc + ?Sized> KnownAdmissionReceiptResolver<'a, R> {
    /// Wrap an already authenticated RPC transport for an existing gateway.
    #[must_use]
    pub const fn new(rpc: &'a R) -> Self {
        Self { rpc }
    }
}

impl<R: Rpc + ?Sized> SubmissionResolver for KnownAdmissionReceiptResolver<'_, R> {
    /// Fetch the existing receipt or return indeterminate without guessing.
    fn resolve(&self, record: &AdmissionRecord) -> Result<LookupResult, ResolveError> {
        let Some(admission_id) = record.admission_id.as_deref() else {
            return Ok(LookupResult::new(LookupDisposition::Indeterminate, None)?);
        };

        let params = BTreeMap::from([
            ("session_id".to_owned(), record.session_id.clone()),
            ("admission_id".to_owned(), admission_id.to_owned()),
        ]);
        let response = self.rpc.call("prompt.receipt", &params)?;
        let receipt = parse_receipt(&response, Some(admission_id))?;
        Ok(LookupResult::new(LookupDisposition::Found, Some(receipt))?)
    }
}

impl<R: Rpc + ?Sized> std::fmt::Debug for KnownAdmissionReceiptResolver<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KnownAdmissionReceiptResolver")
            .field("injected_dependencies", &true)
            .finish_non_exhaustive()
    }
}
