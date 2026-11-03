#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Attach-only gateway discovery tests; no Hermes runtime is started.

use std::cell::RefCell;
use std::collections::BTreeMap;

use hermes_harness::gateway::gateway_attachment::{
    DiscoveryResult, GatewayAttachmentConfig, GatewayAttachmentError, GatewayRpc, GatewayRpcError,
    attach_existing_gateway,
};
use serde_json::{Value, json};

struct StubRpc;

impl GatewayRpc for StubRpc {
    fn rpc(
        &mut self,
        _method: &str,
        _params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        Ok(json!({"ok": true}))
    }
}

type Log = RefCell<Vec<String>>;
type Config<'a> = GatewayAttachmentConfig<'a, u32, StubRpc>;

fn result(endpoint: Option<u32>, state: &str) -> DiscoveryResult<u32> {
    DiscoveryResult {
        endpoint,
        state: Some(state.to_owned()),
    }
}

fn connector(calls: &Log) -> Box<dyn Fn(u32) -> Result<StubRpc, GatewayRpcError> + '_> {
    Box::new(move |endpoint| {
        calls.borrow_mut().push(format!("connect:{endpoint}"));
        Ok(StubRpc)
    })
}

fn run_context(config: &Config<'_>) -> Result<(), GatewayAttachmentError> {
    attach_existing_gateway(config, |_rpc| {
        panic!("non-attachable gateway must not connect");
    })
}

#[test]
fn attaches_only_to_discovered_endpoint() {
    let calls: Log = RefCell::new(Vec::new());
    let config: Config<'_> = GatewayAttachmentConfig::new(
        "/profile",
        Box::new(|home| {
            calls.borrow_mut().push(format!("discover:{home}"));
            Ok(result(Some(42), "ready"))
        }),
        Box::new(|found| found.state.as_deref() == Some("ready")),
        connector(&calls),
    );
    let reply = attach_existing_gateway(&config, |rpc| rpc.rpc("ping", &BTreeMap::new())).unwrap();
    assert_eq!(reply.unwrap(), json!({"ok": true}));
    assert_eq!(*calls.borrow(), vec!["discover:/profile", "connect:42"]);
}

#[test]
fn refuses_non_attachable_gateway_without_connecting() {
    for found in [result(None, "missing"), result(Some(1), "inactive")] {
        let calls: Log = RefCell::new(Vec::new());
        let config: Config<'_> = GatewayAttachmentConfig::new(
            "/profile",
            Box::new(|_home| {
                calls.borrow_mut().push("discover".to_owned());
                Ok(found.clone())
            }),
            Box::new(|item| item.state.as_deref() == Some("ready")),
            connector(&calls),
        );
        let error = run_context(&config).unwrap_err();
        assert!(error.to_string().contains("no start attempted"));
        assert_eq!(*calls.borrow(), vec!["discover"]);
    }
}

#[test]
fn discovery_exception_does_not_attempt_connector() {
    let calls: Log = RefCell::new(Vec::new());
    let config: Config<'_> = GatewayAttachmentConfig::new(
        "/profile",
        Box::new(|_home| {
            calls.borrow_mut().push("discover".to_owned());
            Err("private detail".to_owned())
        }),
        Box::new(|_item| true),
        connector(&calls),
    );
    let error = run_context(&config).unwrap_err();
    assert!(error.to_string().contains("discovery failed"));
    assert_eq!(*calls.borrow(), vec!["discover"]);
}

#[test]
fn waits_for_absent_then_starting_then_ready() {
    let calls: Log = RefCell::new(Vec::new());
    let states = RefCell::new(vec!["ready", "starting", "absent"]);
    let mut config: Config<'_> = GatewayAttachmentConfig::new(
        "/profile",
        Box::new(|_home| {
            let state = states.borrow_mut().pop().unwrap();
            calls.borrow_mut().push(state.to_owned());
            Ok(result((state == "ready").then_some(9), state))
        }),
        Box::new(|item| item.state.as_deref() == Some("ready")),
        connector(&calls),
    );
    config.readiness_timeout = 1.0;
    config.poll_interval = 0.001;
    let reply = attach_existing_gateway(&config, |rpc| rpc.rpc("ping", &BTreeMap::new())).unwrap();
    assert_eq!(reply.unwrap(), json!({"ok": true}));
    assert_eq!(
        *calls.borrow(),
        vec!["absent", "starting", "ready", "connect:9"]
    );
}

#[test]
fn readiness_timeout_never_connects() {
    let calls: Log = RefCell::new(Vec::new());
    let mut config: Config<'_> = GatewayAttachmentConfig::new(
        "/profile",
        Box::new(|_home| {
            calls.borrow_mut().push("discover".to_owned());
            Ok(result(None, "starting"))
        }),
        Box::new(|_item| false),
        connector(&calls),
    );
    config.readiness_timeout = 0.01;
    config.poll_interval = 0.001;
    let error = run_context(&config).unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(calls.borrow().iter().any(|call| call == "discover"));
    assert!(
        !calls
            .borrow()
            .iter()
            .any(|call| call.starts_with("connect"))
    );
}

#[test]
fn permanent_inaccessible_fails_without_polling() {
    let calls: Log = RefCell::new(Vec::new());
    let config: Config<'_> = GatewayAttachmentConfig::new(
        "/profile",
        Box::new(|_home| {
            calls.borrow_mut().push("discover".to_owned());
            Ok(result(None, "inaccessible"))
        }),
        Box::new(|_item| false),
        connector(&calls),
    );
    let error = run_context(&config).unwrap_err();
    assert!(error.to_string().contains("no start attempted"));
    assert_eq!(*calls.borrow(), vec!["discover"]);
}
