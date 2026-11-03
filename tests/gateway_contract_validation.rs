#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline safety checks for existing-gateway contract validation.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use hermes_harness::gateway::gateway_attachment::{
    DiscoveryResult, GatewayAttachmentConfig, GatewayRpc, GatewayRpcError,
};
use hermes_harness::gateway::gateway_contract_validation::validate_existing_gateway;
use serde_json::{Value, json};

const MAX_READONLY_RPCS: usize = 5;

type Calls = Rc<RefCell<Vec<(String, BTreeMap<String, Value>)>>>;

struct StubRpc {
    calls: Calls,
    broken: String,
}

impl StubRpc {
    fn replay_response(&self, params: &BTreeMap<String, Value>) -> Value {
        let mut epoch = "epoch-1";
        let mut truncated = !params.contains_key("replay_epoch");
        let mut snapshot_required = truncated;
        if self.broken == "epoch_change" && params.contains_key("replay_epoch") {
            epoch = "epoch-2";
        } else if self.broken == "snapshot_required" && params.contains_key("replay_epoch") {
            truncated = true;
            snapshot_required = true;
        }
        json!({
            "replay_epoch": epoch,
            "epoch": epoch,
            "latest_seq": 0,
            "events": [],
            "truncated": truncated,
            "snapshot_required": snapshot_required,
        })
    }
}

impl GatewayRpc for StubRpc {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        if method == self.broken {
            return Ok(json!([]));
        }
        match method {
            "ping" => Ok(json!({"pong": true})),
            "session.list" => Ok(json!({"scope": "live", "sessions": []})),
            "session.info" => Ok(json!({"stored_session_id": params["session_id"]})),
            "session.events.since" => Ok(self.replay_response(params)),
            _ => panic!("forbidden RPC method"),
        }
    }
}

struct Dependencies {
    available: bool,
    broken: String,
    calls: Calls,
    endpoints: Rc<RefCell<Vec<u32>>>,
}

impl Dependencies {
    fn new(available: bool, broken: &str) -> Self {
        Self {
            available,
            broken: broken.to_owned(),
            calls: Rc::default(),
            endpoints: Rc::default(),
        }
    }

    fn config(&self) -> GatewayAttachmentConfig<'_, u32, StubRpc> {
        let available = self.available;
        let mut config = GatewayAttachmentConfig::new(
            "/home/hermes/.hermes",
            Box::new(move |_home| {
                Ok(DiscoveryResult {
                    endpoint: available.then_some(7),
                    state: Some(if available { "ready" } else { "absent" }.to_owned()),
                })
            }),
            Box::new(|found| found.endpoint.is_some()),
            Box::new(move |endpoint| {
                self.endpoints.borrow_mut().push(endpoint);
                Ok(StubRpc {
                    calls: Rc::clone(&self.calls),
                    broken: self.broken.clone(),
                })
            }),
        );
        config.readiness_timeout = 0.2;
        config.poll_interval = 0.05;
        config
    }

    fn methods(&self) -> Vec<String> {
        self.calls.borrow().iter().map(|(m, _)| m.clone()).collect()
    }
}

#[test]
fn inactive_gateway_is_never_started() {
    let deps = Dependencies::new(false, "");
    let err = validate_existing_gateway(&deps.config(), None).unwrap_err();
    assert!(
        err.to_string()
            .contains("Existing gateway readiness timed out")
    );
    assert!(deps.calls.borrow().is_empty());
    assert!(deps.endpoints.borrow().is_empty());
}

#[test]
fn no_session_id_uses_only_ping_and_list() {
    let deps = Dependencies::new(true, "");
    let report = validate_existing_gateway(&deps.config(), None).unwrap();
    assert!(report.ping_verified);
    assert_eq!(report.sampled_live_session_count, 0);
    assert!(!report.info_verified);
    assert_eq!(report.replay_latest_seq, None);
    assert_eq!(deps.methods(), vec!["ping", "session.list"]);
    assert_eq!(*deps.endpoints.borrow(), vec![7]);
}

#[test]
fn explicit_session_checks_readonly_shapes() {
    let deps = Dependencies::new(true, "");
    let report = validate_existing_gateway(&deps.config(), Some("session-1")).unwrap();
    assert!(report.info_verified);
    assert!(report.replay_epoch_present);
    assert_eq!(report.replay_latest_seq, Some(0));
    assert!(report.replay_consistent);
    assert_eq!(
        deps.methods(),
        vec![
            "ping",
            "session.list",
            "session.info",
            "session.events.since",
            "session.events.since",
        ]
    );
    let calls = deps.calls.borrow();
    let as_json = |index: usize| serde_json::to_value(&calls[index].1).unwrap();
    assert_eq!(
        as_json(calls.len() - 2),
        json!({"session_id": "session-1", "last_sequence": 0})
    );
    assert_eq!(
        as_json(calls.len() - 1),
        json!({"session_id": "session-1", "last_sequence": 0, "replay_epoch": "epoch-1"})
    );
}

#[test]
fn invalid_response_is_rejected() {
    for method in [
        "ping",
        "session.list",
        "session.info",
        "session.events.since",
    ] {
        let deps = Dependencies::new(true, method);
        assert!(validate_existing_gateway(&deps.config(), Some("session-1")).is_err());
        assert!(deps.calls.borrow().len() <= MAX_READONLY_RPCS);
    }
}

#[test]
fn blank_session_refused_before_discovery() {
    let deps = Dependencies::new(true, "");
    let err = validate_existing_gateway(&deps.config(), Some(" ")).unwrap_err();
    assert!(err.to_string().contains("session_id"));
    assert!(deps.endpoints.borrow().is_empty());
}

#[test]
fn unstable_replay_is_rejected() {
    for broken in ["epoch_change", "snapshot_required"] {
        let deps = Dependencies::new(true, broken);
        assert!(validate_existing_gateway(&deps.config(), Some("session-1")).is_err());
        assert_eq!(deps.calls.borrow().len(), MAX_READONLY_RPCS);
    }
}
