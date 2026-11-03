#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline contract tests for bounded read-only Hermes session discovery.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;

use hermes_harness::gateway::gateway_attachment::{
    DiscoveryResult, GatewayAttachmentConfig, GatewayRpc, GatewayRpcError,
};
use hermes_harness::gateway::gateway_session_discovery::{
    SessionDiscoveryError, SessionScope, discover_existing_sessions,
};
use serde_json::{Value, json};

type Calls = Rc<RefCell<Vec<(String, Value)>>>;

struct Rpc {
    response: Option<Value>,
    calls: Calls,
}

impl GatewayRpc for Rpc {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), serde_json::to_value(params).unwrap()));
        self.response.clone().ok_or_else(|| {
            GatewayRpcError::Io(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "disconnected",
            ))
        })
    }
}

struct Dependencies {
    response: Option<Value>,
    available: bool,
    calls: Calls,
    connected: Cell<bool>,
}

impl Dependencies {
    fn new(response: Option<Value>, available: bool) -> Self {
        Self {
            response,
            available,
            calls: Rc::default(),
            connected: Cell::new(false),
        }
    }

    fn config(&self) -> GatewayAttachmentConfig<'_, u32, Rpc> {
        let available = self.available;
        let mut config = GatewayAttachmentConfig::new(
            "/home/hermes/.hermes",
            Box::new(move |_home| {
                Ok(DiscoveryResult {
                    endpoint: available.then_some(9),
                    state: Some(if available { "ready" } else { "absent" }.to_owned()),
                })
            }),
            Box::new(|found| found.endpoint.is_some()),
            Box::new(move |endpoint| {
                self.connected.set(endpoint == 9);
                Ok(Rpc {
                    response: self.response.clone(),
                    calls: Rc::clone(&self.calls),
                })
            }),
        );
        config.readiness_timeout = 0.2;
        config.poll_interval = 0.05;
        config
    }
}

fn listing(scope: &str) -> Value {
    json!({
        "scope": scope,
        "sessions": [{"session_id": "local-001", "running": false, "title": "private"}],
    })
}

#[test]
fn stored_session_projection_excludes_private_metadata() {
    let deps = Dependencies::new(Some(listing("stored")), true);
    let result = discover_existing_sessions(&deps.config(), SessionScope::Stored, 10).unwrap();
    assert!(deps.connected.get());
    assert_eq!(result.scope, SessionScope::Stored);
    assert_eq!(result.sessions.len(), 1);
    assert_eq!(result.sessions[0].session_id, "local-001");
    assert_eq!(
        *deps.calls.borrow(),
        vec![(
            "session.list".to_owned(),
            json!({"scope": "stored", "limit": 10})
        )]
    );
}

#[test]
fn empty_live_listing_is_valid() {
    let deps = Dependencies::new(Some(json!({"scope": "live", "sessions": []})), true);
    let result = discover_existing_sessions(&deps.config(), SessionScope::Live, 1).unwrap();
    assert!(result.sessions.is_empty());
    assert_eq!(
        *deps.calls.borrow(),
        vec![(
            "session.list".to_owned(),
            json!({"scope": "live", "limit": 1})
        )]
    );
}

#[test]
fn invalid_or_unauthorized_listing_fails_closed() {
    let row = json!({"session_id": "a", "running": false});
    let bad = [
        json!({"scope": "live", "sessions": []}),
        json!({"scope": "stored", "sessions": [{"session_id": "a", "running": "no"}]}),
        json!({"scope": "stored", "sessions": [{"session_id": "", "running": false}]}),
        json!({"scope": "stored", "sessions": [row.clone(), row]}),
        json!({"scope": "stored", "sessions": "not-a-list"}),
    ];
    for response in bad {
        let deps = Dependencies::new(Some(response), true);
        let err = discover_existing_sessions(&deps.config(), SessionScope::Stored, 10).unwrap_err();
        assert!(matches!(err, SessionDiscoveryError::Read(_)));
        assert_eq!(deps.calls.borrow().len(), 1);
    }
}

#[test]
fn oversized_listing_is_rejected() {
    let rows = json!([
        {"session_id": "a", "running": false},
        {"session_id": "b", "running": false},
    ]);
    let deps = Dependencies::new(Some(json!({"scope": "stored", "sessions": rows})), true);
    let err = discover_existing_sessions(&deps.config(), SessionScope::Stored, 1).unwrap_err();
    assert!(matches!(err, SessionDiscoveryError::Read(_)));
    assert_eq!(deps.calls.borrow().len(), 1);
}

#[test]
fn invalid_limit_refused_before_discovery() {
    for limit in [0, 21] {
        let deps = Dependencies::new(Some(listing("stored")), true);
        let err =
            discover_existing_sessions(&deps.config(), SessionScope::Stored, limit).unwrap_err();
        assert!(err.to_string().contains("limit must be an integer"));
        assert!(deps.calls.borrow().is_empty());
    }
}

#[test]
fn absent_gateway_does_not_connect() {
    let deps = Dependencies::new(Some(listing("stored")), false);
    let err = discover_existing_sessions(&deps.config(), SessionScope::Stored, 10).unwrap_err();
    assert!(matches!(err, SessionDiscoveryError::Attachment(_)));
    assert!(!deps.connected.get());
}

#[test]
fn disconnect_is_propagated_without_retry() {
    let deps = Dependencies::new(None, true);
    let err = discover_existing_sessions(&deps.config(), SessionScope::Stored, 10).unwrap_err();
    assert!(matches!(
        err,
        SessionDiscoveryError::Rpc(GatewayRpcError::Io(_))
    ));
    assert_eq!(deps.calls.borrow().len(), 1);
}
