#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use hermes_harness::core::reconciliation::{
    LookupDisposition, ReconciliationStatus, SubmissionResolver, reconcile_unknown,
};
use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_identity::{Receipt, Rpc, Submission};
use hermes_harness::gateway::gateway_resolver::KnownAdmissionReceiptResolver;
use serde_json::{Value, json};

type Call = (String, BTreeMap<String, String>);

/// Scripted RPC transport; no gateway or network is started.
struct FakeRpc {
    response: Value,
    error: Option<&'static str>,
    calls: RefCell<Vec<Call>>,
}

impl FakeRpc {
    fn ok(response: Value) -> Self {
        Self {
            response,
            error: None,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Rpc for FakeRpc {
    fn call(&self, method: &str, params: &BTreeMap<String, String>) -> Result<Value, io::Error> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        self.error.map_or_else(
            || Ok(self.response.clone()),
            |text| Err(io::Error::other(text)),
        )
    }
}

fn make_unknown_store(tmp: &Path, known_admission: bool) -> AdmissionStore {
    let store = AdmissionStore::new(&tmp.join("private").join("admissions.sqlite3")).unwrap();
    store
        .prepare(&Submission::new("session-1", "sub-1"))
        .unwrap();
    if known_admission {
        let receipt = Receipt {
            admission_id: "adm-1".to_owned(),
            status: "queued".to_owned(),
            execution_generation: None,
        };
        store.record_receipt("sub-1", &receipt, false).unwrap();
    }
    store.mark_outcome_unknown("sub-1").unwrap();
    store
}

#[test]
fn unknown_admission_id_does_not_make_rpc_call() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_unknown_store(tmp.path(), false);
    let record = store.get("sub-1").unwrap().unwrap();
    let rpc = FakeRpc::ok(json!({"admission_id": "adm-1", "status": "queued"}));

    let result = KnownAdmissionReceiptResolver::new(&rpc)
        .resolve(&record)
        .unwrap();

    assert_eq!(result.disposition(), LookupDisposition::Indeterminate);
    assert_eq!(rpc.calls.borrow().len(), 0);
}

#[test]
fn known_admission_uses_receipt_rpc_and_validates_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_unknown_store(tmp.path(), true);
    let record = store.get("sub-1").unwrap().unwrap();
    let rpc = FakeRpc::ok(json!({
        "admission_id": "adm-1", "status": "running", "execution_generation": 3
    }));

    let result = KnownAdmissionReceiptResolver::new(&rpc)
        .resolve(&record)
        .unwrap();

    assert_eq!(result.disposition(), LookupDisposition::Found);
    assert_eq!(
        result.receipt().cloned(),
        Some(Receipt {
            admission_id: "adm-1".to_owned(),
            status: "running".to_owned(),
            execution_generation: Some(3),
        })
    );
    let expected = BTreeMap::from([
        ("session_id".to_owned(), "session-1".to_owned()),
        ("admission_id".to_owned(), "adm-1".to_owned()),
    ]);
    assert_eq!(
        *rpc.calls.borrow(),
        vec![("prompt.receipt".to_owned(), expected)]
    );
}

#[test]
fn changed_admission_identity_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_unknown_store(tmp.path(), true);
    let record = store.get("sub-1").unwrap().unwrap();
    let rpc = FakeRpc::ok(json!({"admission_id": "other", "status": "queued"}));

    let err = KnownAdmissionReceiptResolver::new(&rpc)
        .resolve(&record)
        .unwrap_err();
    assert!(err.to_string().contains("changed admission identity"));
}

#[test]
fn rpc_disconnect_preserves_unknown_record() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_unknown_store(tmp.path(), true);
    let mut rpc = FakeRpc::ok(Value::Null);
    rpc.error = Some("simulated disconnect");
    let resolver = KnownAdmissionReceiptResolver::new(&rpc);

    let err = reconcile_unknown(&store, "sub-1", &resolver).unwrap_err();
    assert!(err.to_string().contains("simulated disconnect"));

    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
}

#[test]
fn known_receipt_can_be_reconciled_without_resubmission() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_unknown_store(tmp.path(), true);
    let rpc = FakeRpc::ok(json!({"admission_id": "adm-1", "status": "queued"}));

    let result =
        reconcile_unknown(&store, "sub-1", &KnownAdmissionReceiptResolver::new(&rpc)).unwrap();

    assert_eq!(result, ReconciliationStatus::ReceiptRecorded);
    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::ReceiptObserved);
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
    assert_eq!(rpc.calls.borrow().len(), 1);
}
