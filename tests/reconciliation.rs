#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::Cell;
use std::path::Path;

use hermes_harness::core::reconciliation::{
    LookupDisposition, LookupResult, ReconciliationError, ReconciliationStatus, ResolveError,
    SubmissionResolver, reconcile_unknown,
};
use hermes_harness::gateway::admission_store::{AdmissionRecord, AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_identity::{Receipt, Submission};

/// Deterministic fake for the injected verified lookup contract.
struct Resolver {
    result: LookupResult,
    calls: Cell<usize>,
}

impl Resolver {
    fn new(result: LookupResult) -> Self {
        Self {
            result,
            calls: Cell::new(0),
        }
    }
}

impl SubmissionResolver for Resolver {
    fn resolve(&self, record: &AdmissionRecord) -> Result<LookupResult, ResolveError> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(record.submission_id, "sub-1");
        Ok(self.result.clone())
    }
}

fn receipt(id: &str, status: &str, generation: Option<u64>) -> Receipt {
    Receipt {
        admission_id: id.to_owned(),
        status: status.to_owned(),
        execution_generation: generation,
    }
}

fn uncertain_store(tmp: &Path) -> AdmissionStore {
    let store = AdmissionStore::new(&tmp.join("private").join("admissions.sqlite3")).unwrap();
    store
        .prepare(&Submission::new("session-1", "sub-1"))
        .unwrap();
    store.mark_outcome_unknown("sub-1").unwrap();
    store
}

#[test]
fn found_receipt_is_persisted_as_reconciled() {
    let tmp = tempfile::tempdir().unwrap();
    let store = uncertain_store(tmp.path());
    let resolver = Resolver::new(
        LookupResult::new(
            LookupDisposition::Found,
            Some(receipt("adm-1", "queued", Some(2))),
        )
        .unwrap(),
    );

    let result = reconcile_unknown(&store, "sub-1", &resolver).unwrap();

    assert_eq!(result, ReconciliationStatus::ReceiptRecorded);
    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::ReceiptObserved);
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
    assert_eq!(resolver.calls.get(), 1);
}

#[test]
fn missing_or_ambiguous_lookup_keeps_unknown() {
    for disposition in [
        LookupDisposition::NotFound,
        LookupDisposition::Indeterminate,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let store = uncertain_store(tmp.path());
        let resolver = Resolver::new(LookupResult::new(disposition, None).unwrap());

        let result = reconcile_unknown(&store, "sub-1", &resolver).unwrap();

        assert_eq!(result, ReconciliationStatus::StillUnknown);
        let record = store.get("sub-1").unwrap().unwrap();
        assert_eq!(record.local_state, LocalState::OutcomeUnknown);
        assert_eq!(record.admission_id, None);
        assert_eq!(resolver.calls.get(), 1);
    }
}

#[test]
fn known_admission_identity_cannot_change() {
    let tmp = tempfile::tempdir().unwrap();
    let store = uncertain_store(tmp.path());
    store
        .record_receipt("sub-1", &receipt("adm-original", "queued", None), false)
        .unwrap();
    store.mark_outcome_unknown("sub-1").unwrap();
    let resolver = Resolver::new(
        LookupResult::new(
            LookupDisposition::Found,
            Some(receipt("adm-other", "queued", None)),
        )
        .unwrap(),
    );

    let err = reconcile_unknown(&store, "sub-1", &resolver).unwrap_err();
    assert!(matches!(err, ReconciliationError::Reconciliation(_)));
    assert!(err.to_string().contains("conflicts"));

    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.admission_id.as_deref(), Some("adm-original"));
}

#[test]
fn only_unknown_records_are_eligible() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AdmissionStore::new(&tmp.path().join("admissions.sqlite3")).unwrap();
    store
        .prepare(&Submission::new("session-1", "sub-1"))
        .unwrap();
    let resolver =
        Resolver::new(LookupResult::new(LookupDisposition::Indeterminate, None).unwrap());

    let result = reconcile_unknown(&store, "sub-1", &resolver).unwrap();

    assert_eq!(result, ReconciliationStatus::NotEligible);
    assert_eq!(resolver.calls.get(), 0);
}

#[test]
fn unknown_submission_is_not_eligible() {
    let tmp = tempfile::tempdir().unwrap();
    let store = uncertain_store(tmp.path());
    let resolver =
        Resolver::new(LookupResult::new(LookupDisposition::Indeterminate, None).unwrap());

    let result = reconcile_unknown(&store, "missing", &resolver).unwrap();

    assert_eq!(result, ReconciliationStatus::NotEligible);
    assert_eq!(resolver.calls.get(), 0);
}

#[test]
fn invalid_lookup_result_shape_is_rejected() {
    let err = LookupResult::new(LookupDisposition::Found, None).unwrap_err();
    assert!(err.to_string().contains("receipt is required"));

    let err = LookupResult::new(
        LookupDisposition::NotFound,
        Some(receipt("adm-1", "queued", None)),
    )
    .unwrap_err();
    assert!(err.to_string().contains("receipt is required"));
}

#[test]
fn resolver_exception_preserves_unknown_record() {
    struct FailingResolver;
    impl SubmissionResolver for FailingResolver {
        fn resolve(&self, _record: &AdmissionRecord) -> Result<LookupResult, ResolveError> {
            Err(Box::new(std::io::Error::other("simulated disconnect")))
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let store = uncertain_store(tmp.path());

    let err = reconcile_unknown(&store, "sub-1", &FailingResolver).unwrap_err();
    assert!(err.to_string().contains("simulated disconnect"));

    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
}
