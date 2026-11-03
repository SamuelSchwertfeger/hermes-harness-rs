#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use hermes_harness::core::reconciliation::ReconciliationStatus;
use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_attachment::{
    DiscoveryResult, GatewayAttachmentConfig, GatewayRpc, GatewayRpcError,
};
use hermes_harness::gateway::gateway_identity::Submission;
use hermes_harness::gateway::gateway_live_recovery::recover_with_existing_gateway;
use serde_json::{Value, json};

type Calls = Rc<RefCell<Vec<String>>>;

struct FakeRpc {
    pending: Value,
    receipt_id: &'static str,
    calls: Calls,
}

impl GatewayRpc for FakeRpc {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls.borrow_mut().push(method.to_owned());
        match method {
            "session.events.since" if !params.contains_key("replay_epoch") => Ok(json!({
                "events": [], "latest_seq": 1, "last_sequence": 1,
                "epoch": "epoch-1", "replay_epoch": "epoch-1",
                "truncated": true, "snapshot_required": true, "count": 0,
            })),
            "session.events.since" => Ok(json!({
                "events": [{
                    "type": "session.info", "session_id": "session-1",
                    "payload": {"pending": self.pending}, "seq": 1,
                    "replay_epoch": "epoch-1",
                }],
                "latest_seq": 1, "last_sequence": 1,
                "epoch": "epoch-1", "replay_epoch": "epoch-1",
                "truncated": false, "snapshot_required": false, "count": 1,
            })),
            "prompt.receipt" => Ok(json!({"admission_id": self.receipt_id, "status": "queued"})),
            _ => panic!("unexpected RPC: {method}"),
        }
    }
}

struct FakeGateway {
    rpc_calls: Calls,
    discovery_calls: Rc<RefCell<Vec<String>>>,
    connector_endpoints: Rc<RefCell<Vec<String>>>,
    pending: Value,
}

impl FakeGateway {
    fn new(pending: Value) -> Self {
        Self {
            rpc_calls: Rc::default(),
            discovery_calls: Rc::default(),
            connector_endpoints: Rc::default(),
            pending,
        }
    }

    fn config(&self, profile_home: &str) -> GatewayAttachmentConfig<'static, String, FakeRpc> {
        let discovery_calls = Rc::clone(&self.discovery_calls);
        let endpoints = Rc::clone(&self.connector_endpoints);
        let rpc_calls = Rc::clone(&self.rpc_calls);
        let pending = self.pending.clone();
        GatewayAttachmentConfig::new(
            profile_home,
            Box::new(move |home| {
                discovery_calls.borrow_mut().push(home.to_owned());
                Ok(DiscoveryResult {
                    endpoint: Some("endpoint-1".to_owned()),
                    state: Some("ready".to_owned()),
                })
            }),
            Box::new(|_| true),
            Box::new(move |endpoint| {
                endpoints.borrow_mut().push(endpoint);
                Ok(FakeRpc {
                    pending: pending.clone(),
                    receipt_id: "adm-1",
                    calls: Rc::clone(&rpc_calls),
                })
            }),
        )
    }
}

fn unknown_store(path: &Path) -> AdmissionStore {
    let store = AdmissionStore::new(&path.join("admissions.sqlite")).unwrap();
    store
        .prepare(&Submission::new("session-1", "sub-1"))
        .unwrap();
    store.mark_outcome_unknown("sub-1").unwrap();
    store
}

#[test]
fn live_recovery_attaches_once_and_verifies_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    let store = unknown_store(tmp.path());
    let gateway = FakeGateway::new(json!([{"input_id": "sub-1", "admission_id": "adm-1"}]));
    let home = tmp.path().to_string_lossy().into_owned();

    let status = recover_with_existing_gateway(&store, "sub-1", &gateway.config(&home)).unwrap();

    assert_eq!(status, ReconciliationStatus::ReceiptRecorded);
    let record = store.get("sub-1").unwrap().unwrap();
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
    assert_ne!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(*gateway.discovery_calls.borrow(), [home]);
    assert_eq!(*gateway.connector_endpoints.borrow(), ["endpoint-1"]);
    assert_eq!(
        *gateway.rpc_calls.borrow(),
        [
            "session.events.since",
            "session.events.since",
            "prompt.receipt"
        ]
    );
}

#[test]
fn ineligible_record_does_not_discover_or_connect() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AdmissionStore::new(&tmp.path().join("admissions.sqlite")).unwrap();
    let gateway = FakeGateway::new(json!([]));
    let home = tmp.path().to_string_lossy().into_owned();

    let status = recover_with_existing_gateway(&store, "missing", &gateway.config(&home)).unwrap();

    assert_eq!(status, ReconciliationStatus::NotEligible);
    assert_eq!(gateway.discovery_calls.borrow().len(), 0);
    assert_eq!(gateway.connector_endpoints.borrow().len(), 0);
    assert_eq!(gateway.rpc_calls.borrow().len(), 0);
}
