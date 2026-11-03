#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Scripted gateway admission lifecycle tests: never contacts an actual gateway.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use hermes_harness::gateway::gateway_lifecycle::{
    AdmissionError, AdmissionState, GatewayLifecycle, GatewayRpc, LifecycleConfig,
};
use serde_json::{Value, json};

#[derive(Default)]
struct ScriptedRpc {
    calls: Vec<(String, BTreeMap<String, String>)>,
    fail: BTreeSet<String>,
}

impl GatewayRpc for ScriptedRpc {
    fn call(&mut self, method: &str, params: &BTreeMap<String, String>) -> io::Result<Value> {
        self.calls.push((method.to_owned(), params.clone()));
        if self.fail.contains(method) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "simulated disconnect",
            ));
        }
        Ok(json!({"accepted": true, "method": method}))
    }
}

#[test]
fn submit_records_private_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let mut rpc = ScriptedRpc::default();
    let mut client = GatewayLifecycle::new(&mut rpc, LifecycleConfig::new(tmp.path())).unwrap();
    let initial = client.new_admission("session-1").unwrap();
    let result = client.submit(&initial, "hello").unwrap();
    assert_eq!(result.state, AdmissionState::ReceiptRecorded);
    assert_eq!(rpc.calls[0].1["submission_id"], initial.submission_id);
    let evidence = tmp.path().join(format!("{}.json", initial.submission_id));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&evidence).unwrap()).unwrap();
    assert_eq!(saved["state"], "receipt_recorded");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let expected_permissions = 0o600;
        let mode = std::fs::metadata(&evidence).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, expected_permissions);
    }
}

#[test]
fn disconnect_never_resubmits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut rpc = ScriptedRpc::default();
    rpc.fail.insert("prompt.submit".to_owned());
    let mut client = GatewayLifecycle::new(&mut rpc, LifecycleConfig::new(tmp.path())).unwrap();
    let fresh = client.new_admission("session-1").unwrap();
    let unknown = client.submit(&fresh, "hello").unwrap();
    assert_eq!(unknown.state, AdmissionState::SubmitUnknown);
    let reconciled = client.receipt(&unknown).unwrap();
    assert_eq!(reconciled.state, AdmissionState::ReceiptRecorded);
    let methods: Vec<&str> = rpc.calls.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(methods, ["prompt.submit", "prompt.receipt"]);
}

#[test]
fn cancel_is_request_not_termination() {
    let tmp = tempfile::tempdir().unwrap();
    let mut rpc = ScriptedRpc::default();
    let mut client = GatewayLifecycle::new(&mut rpc, LifecycleConfig::new(tmp.path())).unwrap();
    let fresh = client.new_admission("session-1").unwrap();
    let submitted = client.submit(&fresh, "hello").unwrap();
    let cancelled = client.cancel(&submitted, "admission-1").unwrap();
    assert_eq!(cancelled.state, AdmissionState::CancelRequested);
    assert_ne!(cancelled.state, AdmissionState::TerminalUnverified);
}

#[test]
fn cancel_disconnect_is_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut rpc = ScriptedRpc::default();
    rpc.fail.insert("prompt.cancel".to_owned());
    let mut client = GatewayLifecycle::new(&mut rpc, LifecycleConfig::new(tmp.path())).unwrap();
    let fresh = client.new_admission("session-1").unwrap();
    let submitted = client.submit(&fresh, "hello").unwrap();
    assert_eq!(
        client.cancel(&submitted, "admission-1").unwrap().state,
        AdmissionState::CancelUnknown
    );
}

#[test]
fn live_transport_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut rpc = ScriptedRpc::default();
    let mut config = LifecycleConfig::new(tmp.path());
    config.simulated_only = false;
    let err = GatewayLifecycle::new(&mut rpc, config).err().unwrap();
    assert!(matches!(err, AdmissionError::Rejected(_)));
    assert!(err.to_string().contains("Live gateway"));
}

#[test]
fn invalid_transitions_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut rpc = ScriptedRpc::default();
    let mut client = GatewayLifecycle::new(&mut rpc, LifecycleConfig::new(tmp.path())).unwrap();
    let initial = client.new_admission("session-1").unwrap();
    assert!(client.cancel(&initial, "admission-1").is_err());
    assert!(client.submit(&initial, "").is_err());
}
