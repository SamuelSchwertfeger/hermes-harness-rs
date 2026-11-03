#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use hermes_harness::core::reconciliation::ReconciliationStatus;
use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_async_recovery::{AsyncRecoveryError, recover_with_async_rpc};
use hermes_harness::gateway::gateway_identity::{Receipt, Submission};
use hermes_harness::gateway::gateway_readonly::{GatewayReadError, GatewayRpc, GatewayRpcError};
use serde_json::{Value, json};

struct FakeRpc {
    pending: Option<Value>,
    pending_id: &'static str,
    receipt: Value,
    fail: &'static str,
    calls: Vec<(String, BTreeMap<String, Value>)>,
}

impl FakeRpc {
    fn new(pending: Value) -> Self {
        Self {
            pending: Some(pending),
            pending_id: "sub-1",
            receipt: json!({"admission_id": "adm-1", "status": "queued"}),
            fail: "",
            calls: Vec::new(),
        }
    }

    fn identity_matrix(receipt: Value) -> Self {
        let mut rpc = Self::new(Value::Null);
        rpc.pending = None;
        rpc.receipt = receipt;
        rpc
    }

    fn methods(&self) -> Vec<&str> {
        self.calls
            .iter()
            .map(|(method, _)| method.as_str())
            .collect()
    }

    fn assert_safe_calls(&self) {
        let methods = self.methods();
        assert!(
            methods
                .iter()
                .all(|method| matches!(*method, "session.events.since" | "prompt.receipt")),
            "unexpected calls: {:?}",
            self.calls
        );
        assert!(methods.iter().filter(|&&m| m == "prompt.receipt").count() <= 1);
        assert!(
            methods
                .iter()
                .filter(|&&m| m == "session.events.since")
                .count()
                <= 2
        );
    }
}

impl GatewayRpc for FakeRpc {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls.push((method.to_owned(), params.clone()));
        if self.fail == method {
            return Err(io::Error::other("connection interrupted").into());
        }
        match method {
            "session.events.since" if !params.contains_key("replay_epoch") => Ok(json!({
                "events": [], "latest_seq": 1, "last_sequence": 1,
                "epoch": "epoch-1", "replay_epoch": "epoch-1",
                "truncated": true, "snapshot_required": true, "count": 0,
            })),
            "session.events.since" => Ok(json!({
                "events": [{
                    "type": "session.info", "session_id": "session-1",
                    "payload": {"pending": self.pending.clone().unwrap_or_else(|| json!([
                        {"input_id": self.pending_id, "admission_id": "adm-1"}
                    ]))}, "seq": 1,
                    "replay_epoch": "epoch-1",
                }],
                "latest_seq": 1, "last_sequence": 1,
                "epoch": "epoch-1", "replay_epoch": "epoch-1",
                "truncated": false, "snapshot_required": false, "count": 1,
            })),
            "prompt.receipt" => Ok(self.receipt.clone()),
            _ => panic!("unexpected method"),
        }
    }
}

fn unknown_store(tmp: &Path, known: bool) -> AdmissionStore {
    let store = AdmissionStore::new(&tmp.join("admissions.sqlite")).unwrap();
    store
        .prepare(&Submission::new("session-1", "sub-1"))
        .unwrap();
    if known {
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
fn unique_pending_candidate_is_receipt_verified() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), false);
    let mut rpc = FakeRpc::new(json!([{"input_id": "sub-1", "admission_id": "adm-1"}]));
    let status = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap();
    assert_eq!(status, ReconciliationStatus::ReceiptRecorded);
    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
    assert_ne!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(
        rpc.methods(),
        [
            "session.events.since",
            "session.events.since",
            "prompt.receipt"
        ]
    );
}

#[test]
fn no_unambiguous_pending_match_stays_unknown() {
    let cases = [
        json!([]),
        Value::Null,
        json!("bad"),
        json!([
            {"input_id": "sub-1", "admission_id": "adm-1"},
            {"input_id": "sub-1", "admission_id": "adm-2"},
        ]),
    ];
    for pending in cases {
        let tmp = tempfile::tempdir().unwrap();
        let store = unknown_store(tmp.path(), false);
        let mut rpc = FakeRpc::new(pending);
        let status = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap();
        assert_eq!(status, ReconciliationStatus::StillUnknown);
        let record = store.get("sub-1").unwrap().unwrap();
        assert_eq!(record.local_state, LocalState::OutcomeUnknown);
        assert_eq!(record.admission_id, None);
        assert_eq!(
            rpc.methods(),
            ["session.events.since", "session.events.since"]
        );
    }
}

#[test]
fn known_id_skips_event_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), true);
    let mut rpc = FakeRpc::new(Value::Null);
    let result = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap();
    assert_eq!(result, ReconciliationStatus::ReceiptRecorded);
    assert_eq!(rpc.methods(), ["prompt.receipt"]);
}

#[test]
fn receipt_identity_mismatch_leaves_known_id_and_unknown_state() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), true);
    let mut rpc = FakeRpc::new(Value::Null);
    rpc.receipt = json!({"admission_id": "other", "status": "queued"});
    let err = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap_err();
    assert!(matches!(err, AsyncRecoveryError::Identity(_)));
    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
}

#[test]
fn disconnect_retains_unknown() {
    for failed_method in ["session.events.since", "prompt.receipt"] {
        let tmp = tempfile::tempdir().unwrap();
        let store = unknown_store(tmp.path(), failed_method == "prompt.receipt");
        let mut rpc = FakeRpc::new(json!([{"input_id": "sub-1", "admission_id": "adm-1"}]));
        rpc.fail = failed_method;
        let err = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap_err();
        if failed_method == "session.events.since" {
            assert!(matches!(
                err,
                AsyncRecoveryError::Read(GatewayReadError::Read(_))
            ));
        } else {
            assert!(matches!(
                err,
                AsyncRecoveryError::Rpc(GatewayRpcError::Io(_))
            ));
        }
        let record = store.get("sub-1").unwrap().unwrap();
        assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    }
}

#[test]
fn ineligible_record_does_not_make_any_rpcs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AdmissionStore::new(&tmp.path().join("admissions.sqlite")).unwrap();
    let mut rpc = FakeRpc::new(json!([]));
    let result = recover_with_async_rpc(&store, "missing", &mut rpc).unwrap();
    assert_eq!(result, ReconciliationStatus::NotEligible);
    assert_eq!(rpc.calls.len(), 0);
}

// The Python coroutine tests use Rust's synchronous injected RPC seam.
#[test]
fn authoritative_receipt_matches_admission_not_submission() {
    for known in [false, true] {
        for status in ["queued", "running", "complete", "unrecognized"] {
            let case = format!("case known={known}, status={status}");
            let tmp = tempfile::tempdir().unwrap();
            let store = unknown_store(tmp.path(), known);
            let mut rpc = FakeRpc::identity_matrix(json!({
                "admission_id": "adm-1", "status": status,
            }));

            let outcome = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap();
            assert_eq!(outcome, ReconciliationStatus::ReceiptRecorded, "{case}");
            let record = store.get("sub-1").unwrap().unwrap();
            assert_eq!(record.submission_id, "sub-1", "{case}");
            assert_eq!(record.admission_id.as_deref(), Some("adm-1"), "{case}");
            assert_eq!(record.gateway_status.as_deref(), Some(status), "{case}");
            assert_eq!(record.local_state, LocalState::ReceiptObserved, "{case}");
            let expected = if known {
                vec!["prompt.receipt"]
            } else {
                vec![
                    "session.events.since",
                    "session.events.since",
                    "prompt.receipt",
                ]
            };
            assert_eq!(rpc.methods(), expected, "{case}");
            let (_, params) = rpc.calls.last().unwrap();
            assert_eq!(
                params,
                &BTreeMap::from([
                    ("session_id".to_owned(), json!("session-1")),
                    ("admission_id".to_owned(), json!("adm-1")),
                ]),
                "{case}"
            );
            rpc.assert_safe_calls();
        }
    }
}

#[test]
fn untrusted_receipt_never_reconciles() {
    for known in [false, true] {
        for receipt in [
            json!({"admission_id": "sub-1", "status": "queued"}),
            json!({"admission_id": "adm-2", "status": "queued"}),
            json!({"admission_id": "adm-1"}),
            json!({"admission_id": "adm-1", "status": ""}),
            json!({"admission_id": "adm-1", "status": "queued", "execution_generation": true}),
            json!([]),
        ] {
            let case = format!("case known={known}, receipt={receipt}");
            let tmp = tempfile::tempdir().unwrap();
            let store = unknown_store(tmp.path(), known);
            let before = store.get("sub-1").unwrap().unwrap();
            let mut rpc = FakeRpc::identity_matrix(receipt);

            let err = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap_err();
            assert!(
                matches!(err, AsyncRecoveryError::Identity(_)),
                "{case}: {err}"
            );
            assert!(err.to_string().contains("Gateway"), "{case}: {err}");
            let after = store.get("sub-1").unwrap().unwrap();
            assert_eq!(after.local_state, LocalState::OutcomeUnknown, "{case}");
            assert_eq!(after.admission_id, before.admission_id, "{case}");
            assert_eq!(after.gateway_status, before.gateway_status, "{case}");
            rpc.assert_safe_calls();
        }
    }
}

#[test]
fn receipt_transport_interruption_preserves_identity() {
    for known in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let store = unknown_store(tmp.path(), known);
        let before = store.get("sub-1").unwrap().unwrap();
        let mut rpc = FakeRpc::identity_matrix(Value::Null);
        rpc.fail = "prompt.receipt";

        let err = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap_err();
        assert!(
            matches!(err, AsyncRecoveryError::Rpc(GatewayRpcError::Io(_))),
            "case known={known}: {err}"
        );
        assert!(
            err.to_string().contains("connection interrupted"),
            "case known={known}: {err}"
        );
        let after = store.get("sub-1").unwrap().unwrap();
        assert_eq!(after.local_state, before.local_state, "case known={known}");
        assert_eq!(
            after.local_state,
            LocalState::OutcomeUnknown,
            "case known={known}"
        );
        assert_eq!(
            after.admission_id, before.admission_id,
            "case known={known}"
        );
        rpc.assert_safe_calls();
    }
}

#[test]
fn event_replay_disconnect_never_attempts_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), false);
    let mut rpc = FakeRpc::identity_matrix(Value::Null);
    rpc.fail = "session.events.since";

    let err = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap_err();
    assert!(matches!(
        err,
        AsyncRecoveryError::Read(GatewayReadError::Read(_))
    ));
    assert!(err.to_string().contains("no retry"), "{err}");
    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(record.admission_id, None);
    assert_eq!(rpc.methods(), ["session.events.since"]);
    rpc.assert_safe_calls();
}

#[test]
fn unrelated_pending_identity_is_not_authority() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path(), false);
    let mut rpc = FakeRpc::identity_matrix(Value::Null);
    rpc.pending_id = "other-submission";

    let outcome = recover_with_async_rpc(&store, "sub-1", &mut rpc).unwrap();
    assert_eq!(outcome, ReconciliationStatus::StillUnknown);
    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.admission_id, None);
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(
        rpc.methods(),
        ["session.events.since", "session.events.since"]
    );
    rpc.assert_safe_calls();
}
