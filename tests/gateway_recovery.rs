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

use hermes_harness::core::reconciliation::ReconciliationStatus;
use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_identity::{Receipt, Rpc, Submission};
use hermes_harness::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use hermes_harness::gateway::gateway_recovery::{
    RecoveryClients, RecoveryError, recover_unknown_admission,
};
use serde_json::{Value, json};

/// Scripted read-only session RPC; fails if an unexpected method is used.
struct ReadRpc {
    pending: Value,
    error: Option<&'static str>,
    calls: Vec<String>,
}

impl ReadRpc {
    fn new(pending: Value) -> Self {
        Self {
            pending,
            error: None,
            calls: Vec::new(),
        }
    }
}

impl GatewayRpc for ReadRpc {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls.push(method.to_owned());
        assert_eq!(method, "session.events.since");
        if let Some(text) = self.error {
            return Err(io::Error::other(text).into());
        }
        if !params.contains_key("replay_epoch") {
            return Ok(json!({
                "events": [], "latest_seq": 1, "last_sequence": 1,
                "epoch": "epoch-1", "replay_epoch": "epoch-1",
                "truncated": true, "snapshot_required": true, "count": 0,
            }));
        }
        Ok(json!({
            "events": [{
                "type": "session.info", "session_id": "session-1",
                "payload": {"pending": self.pending}, "seq": 1,
                "replay_epoch": "epoch-1",
            }],
            "latest_seq": 1, "last_sequence": 1,
            "epoch": "epoch-1", "replay_epoch": "epoch-1",
            "truncated": false, "snapshot_required": false, "count": 1,
        }))
    }
}

/// Scripted known-admission receipt RPC; never accepts `prompt.submit`.
struct ReceiptRpc {
    response: Value,
    error: Option<&'static str>,
    calls: RefCell<Vec<(String, BTreeMap<String, String>)>>,
}

impl ReceiptRpc {
    fn new(response: Value) -> Self {
        Self {
            response,
            error: None,
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Rpc for ReceiptRpc {
    fn call(&self, method: &str, params: &BTreeMap<String, String>) -> Result<Value, io::Error> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        assert_eq!(method, "prompt.receipt");
        self.error.map_or_else(
            || Ok(self.response.clone()),
            |text| Err(io::Error::other(text)),
        )
    }
}

/// Prepare a durable submission whose transport outcome is uncertain.
fn unknown_store(tmp: &Path, admission_id: Option<&str>) -> AdmissionStore {
    let store = AdmissionStore::new(&tmp.join("private").join("admissions.sqlite3")).unwrap();
    store
        .prepare(&Submission::new("session-1", "sub-1"))
        .unwrap();
    if let Some(id) = admission_id {
        let receipt = Receipt {
            admission_id: id.to_owned(),
            status: "queued".to_owned(),
            execution_generation: None,
        };
        store.record_receipt("sub-1", &receipt, false).unwrap();
    }
    store.mark_outcome_unknown("sub-1").unwrap();
    store
}

#[test]
fn unique_pending_candidate_is_receipt_verified() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), None);
    let mut reader = ReadRpc::new(json!([{"input_id": "sub-1", "admission_id": "adm-1"}]));
    let receipts = ReceiptRpc::new(
        json!({"admission_id": "adm-1", "status": "started", "execution_generation": 2}),
    );

    let result = recover_unknown_admission(
        &store,
        "sub-1",
        &mut RecoveryClients {
            read_rpc: &mut reader,
            receipt_rpc: &receipts,
        },
    )
    .unwrap();

    assert_eq!(result, ReconciliationStatus::ReceiptRecorded);
    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::ReceiptObserved);
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
    assert_eq!(record.gateway_status.as_deref(), Some("started"));
    assert_eq!(
        reader.calls,
        ["session.events.since", "session.events.since"]
    );
    let expected = BTreeMap::from([
        ("session_id".to_owned(), "session-1".to_owned()),
        ("admission_id".to_owned(), "adm-1".to_owned()),
    ]);
    assert_eq!(
        *receipts.calls.borrow(),
        vec![("prompt.receipt".to_owned(), expected)]
    );
}

#[test]
fn no_unambiguous_pending_match_stays_unknown() {
    let cases = [
        json!([]),
        Value::Null,
        json!("bad"),
        json!([42]),
        json!([
            {"input_id": "sub-1", "admission_id": "a"},
            {"input_id": "sub-1", "admission_id": "b"},
        ]),
    ];
    for pending in cases {
        let tmp = tempfile::tempdir().unwrap();
        let store = unknown_store(tmp.path(), None);
        let mut reader = ReadRpc::new(pending);
        let receipts = ReceiptRpc::new(json!({"admission_id": "a", "status": "queued"}));

        let result = recover_unknown_admission(
            &store,
            "sub-1",
            &mut RecoveryClients {
                read_rpc: &mut reader,
                receipt_rpc: &receipts,
            },
        )
        .unwrap();

        assert_eq!(result, ReconciliationStatus::StillUnknown);
        assert_eq!(receipts.calls.borrow().len(), 0);
        let record = store.get("sub-1").unwrap().unwrap();
        assert_eq!(record.local_state, LocalState::OutcomeUnknown);
        assert_eq!(record.admission_id, None);
    }
}

#[test]
fn known_id_skips_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), Some("adm-1"));
    let mut reader = ReadRpc::new(json!([]));
    let receipts = ReceiptRpc::new(json!({"admission_id": "adm-1", "status": "queued"}));

    let result = recover_unknown_admission(
        &store,
        "sub-1",
        &mut RecoveryClients {
            read_rpc: &mut reader,
            receipt_rpc: &receipts,
        },
    )
    .unwrap();

    assert_eq!(result, ReconciliationStatus::ReceiptRecorded);
    assert_eq!(reader.calls.len(), 0);
    assert_eq!(receipts.calls.borrow().len(), 1);
}

#[test]
fn mismatched_receipt_cannot_persist_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), None);
    let mut reader = ReadRpc::new(json!([{"input_id": "sub-1", "admission_id": "adm-1"}]));
    let receipts = ReceiptRpc::new(json!({"admission_id": "different", "status": "queued"}));

    let err = recover_unknown_admission(
        &store,
        "sub-1",
        &mut RecoveryClients {
            read_rpc: &mut reader,
            receipt_rpc: &receipts,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("changed admission identity"));

    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(record.admission_id, None);
}

#[test]
fn receipt_disconnect_retains_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), None);
    let mut reader = ReadRpc::new(json!([{"input_id": "sub-1", "admission_id": "adm-1"}]));
    let mut receipts = ReceiptRpc::new(Value::Null);
    receipts.error = Some("disconnected");

    let err = recover_unknown_admission(
        &store,
        "sub-1",
        &mut RecoveryClients {
            read_rpc: &mut reader,
            receipt_rpc: &receipts,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("disconnected"));

    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
}

#[test]
fn read_disconnect_retains_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), None);
    let mut reader = ReadRpc::new(json!([]));
    reader.error = Some("disconnected");
    let receipts = ReceiptRpc::new(Value::Null);

    let err = recover_unknown_admission(
        &store,
        "sub-1",
        &mut RecoveryClients {
            read_rpc: &mut reader,
            receipt_rpc: &receipts,
        },
    )
    .unwrap_err();
    assert!(matches!(err, RecoveryError::Read(_)));
    assert!(err.to_string().contains("event replay failed"));

    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(receipts.calls.borrow().len(), 0);
}

#[test]
fn ineligible_does_not_make_any_rpcs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AdmissionStore::new(&tmp.path().join("admissions.sqlite3")).unwrap();
    store
        .prepare(&Submission::new("session-1", "sub-1"))
        .unwrap();
    let mut reader = ReadRpc::new(json!([]));
    let receipts = ReceiptRpc::new(Value::Null);

    let result = recover_unknown_admission(
        &store,
        "sub-1",
        &mut RecoveryClients {
            read_rpc: &mut reader,
            receipt_rpc: &receipts,
        },
    )
    .unwrap();

    assert_eq!(result, ReconciliationStatus::NotEligible);
    assert_eq!(reader.calls.len(), 0);
    assert_eq!(receipts.calls.borrow().len(), 0);
}
