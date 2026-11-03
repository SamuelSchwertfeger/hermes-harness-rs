#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline tests for the injected, read-only Hermes connection boundary.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::rc::Rc;

use hermes_harness::gateway::gateway_readonly::{
    GatewayReadError, GatewayRpc, GatewayRpcError, read_from_hermes, read_session,
};
use serde_json::{Value, json};

type Call = (String, BTreeMap<String, Value>);
type Calls = Rc<RefCell<Vec<Call>>>;

struct FakeRpc {
    calls: Calls,
    response: Value,
    failure: bool,
}

impl FakeRpc {
    fn new(response: Value) -> Self {
        Self {
            calls: Rc::default(),
            response,
            failure: false,
        }
    }
}

impl GatewayRpc for FakeRpc {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        if self.failure {
            return Err(io::Error::new(io::ErrorKind::ConnectionReset, "disconnect").into());
        }
        Ok(self.response.clone())
    }
}

fn session_params(id: &str) -> BTreeMap<String, Value> {
    BTreeMap::from([("session_id".to_owned(), json!(id))])
}

#[test]
fn read_only_method_and_identity() {
    let mut rpc = FakeRpc::new(json!({"title": "isolated"}));
    let result = read_session(&mut rpc, "session-123").unwrap();
    assert_eq!(result.session_id, "session-123");
    assert_eq!(Value::Object(result.data), json!({"title": "isolated"}));
    assert_eq!(
        *rpc.calls.borrow(),
        vec![("session.info".to_owned(), session_params("session-123"))]
    );
}

#[test]
fn invalid_session_refused_without_rpc() {
    let mut rpc = FakeRpc::new(json!({}));
    let error = read_session(&mut rpc, " ").unwrap_err();
    assert!(matches!(error, GatewayReadError::InvalidArgument(_)));
    assert!(error.to_string().contains("session_id"));
    assert!(rpc.calls.borrow().is_empty());
}

#[test]
fn invalid_payload_rejected() {
    // JSON objects always have string keys; a non-object payload is the invalid shape.
    let mut rpc = FakeRpc::new(json!([[1, "invalid"]]));
    let error = read_session(&mut rpc, "session-123").unwrap_err();
    assert!(matches!(error, GatewayReadError::Read(_)));
    assert!(error.to_string().contains("string keys"));
}

#[test]
fn disconnect_never_retries() {
    let mut rpc = FakeRpc::new(Value::Null);
    rpc.failure = true;
    let error = read_session(&mut rpc, "session-123").unwrap_err();
    assert!(matches!(error, GatewayReadError::Read(_)));
    assert!(error.to_string().contains("no retry"));
    assert_eq!(rpc.calls.borrow().len(), 1);
}

#[test]
fn no_endpoint_does_not_invoke_connector() {
    let endpoints: RefCell<Vec<u32>> = RefCell::new(Vec::new());
    let error = read_from_hermes(None::<u32>, "session-123", |endpoint| {
        endpoints.borrow_mut().push(endpoint);
        Ok(FakeRpc::new(json!({})))
    })
    .unwrap_err();
    assert!(matches!(error, GatewayReadError::InvalidArgument(_)));
    assert!(error.to_string().contains("mandatory"));
    assert!(endpoints.borrow().is_empty());
}

#[test]
fn injected_connector_uses_explicit_endpoint_only() {
    let endpoints: RefCell<Vec<u32>> = RefCell::new(Vec::new());
    let calls: Calls = Rc::default();
    let shared = Rc::clone(&calls);
    let result = read_from_hermes(Some(7_u32), "session-123", |endpoint| {
        endpoints.borrow_mut().push(endpoint);
        let mut rpc = FakeRpc::new(json!({"status": "opaque"}));
        rpc.calls = shared;
        Ok(rpc)
    })
    .unwrap();
    assert_eq!(*endpoints.borrow(), vec![7_u32]);
    assert_eq!(result.data["status"], json!("opaque"));
    let methods: Vec<String> = calls.borrow().iter().map(|call| call.0.clone()).collect();
    assert_eq!(methods, vec!["session.info".to_owned()]);
}
