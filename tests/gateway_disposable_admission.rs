#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::path::Path;

use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_disposable_admission::{
    DisposableAdmissionError, DisposableAdmissionRequest, create_and_submit_once,
};
use hermes_harness::gateway::gateway_identity::GatewayIdentityError;
use hermes_harness::gateway::gateway_live_admission::LiveAdmissionError;
use hermes_harness::gateway::gateway_session_creation::{
    RpcFailure, SessionCreationError, SessionCreationRequest, SessionRpc,
};
use serde_json::{Map, Value, json};

const SESSION_ID: &str = "local-aaaaaaaaaaaaaaaa";

/// Injected failure for one RPC method.
enum Failure {
    Response(Value),
    Error(&'static str),
}

/// One shared connection that records all RPC activity and injected failures.
#[derive(Default)]
struct FakeRpc {
    calls: Vec<(String, Map<String, Value>)>,
    failures: BTreeMap<&'static str, Failure>,
}

impl SessionRpc for FakeRpc {
    fn rpc(&mut self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcFailure> {
        self.calls.push((method.to_owned(), params.clone()));
        match self.failures.get(method) {
            Some(Failure::Error(text)) => return Err(std::io::Error::other(*text).into()),
            Some(Failure::Response(value)) => return Ok(value.clone()),
            None => {}
        }
        match method {
            "session.create" => Ok(json!({"session_id": SESSION_ID, "subscription_id": "sub-1"})),
            "prompt.submit" | "prompt.receipt" => {
                Ok(json!({"admission_id": "adm-1", "status": "queued"}))
            }
            _ => panic!("Unexpected RPC: {method}"),
        }
    }
}

impl FakeRpc {
    fn failing(method: &'static str, failure: Failure) -> Self {
        Self {
            calls: Vec::new(),
            failures: BTreeMap::from([(method, failure)]),
        }
    }

    fn methods(&self) -> Vec<&str> {
        self.calls
            .iter()
            .map(|(method, _)| method.as_str())
            .collect()
    }
}

fn make_request(tmp: &Path) -> DisposableAdmissionRequest {
    DisposableAdmissionRequest {
        creation: SessionCreationRequest {
            request_id: "create-1".to_owned(),
            record_path: tmp.join("creation.json"),
        },
        submission_id: "submit-1".to_owned(),
        text: "Reply with M3_OK. Do not use tools.".to_owned(),
    }
}

fn make_store(tmp: &Path) -> AdmissionStore {
    AdmissionStore::new(&tmp.join("admissions.sqlite")).unwrap()
}

#[test]
fn order_and_same_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let request = make_request(tmp.path());
    let mut rpc = FakeRpc::default();

    let result = create_and_submit_once(&mut rpc, &store, &request).unwrap();

    assert_eq!(
        rpc.methods(),
        ["session.create", "prompt.submit", "prompt.receipt"]
    );
    let create = &rpc.calls[0].1;
    assert_eq!(create["request_id"], "create-1");
    assert_eq!(create["source"], "cli");
    assert_eq!(create["toolsets"], json!([]));
    assert_eq!(create["max_turns"], 1);
    assert_eq!(create["run_budget"], 60);
    assert!(Path::new(create["cwd"].as_str().unwrap()).is_dir());
    assert_eq!(
        rpc.calls[1].1["session_id"],
        result.prepared.session_id.as_str()
    );
    assert_eq!(rpc.calls[1].1["submission_id"], "submit-1");
    assert_eq!(
        rpc.calls[2].1["admission_id"],
        result.admission.admission_id.as_str()
    );
    assert_eq!(result.prepared.subscription_id, "sub-1");
    assert_eq!(result.admission.admission_id, "adm-1");
    let record = store.get("submit-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::Admitted);
    let written = std::fs::read_to_string(&request.creation.record_path).unwrap();
    let parsed: Value = serde_json::from_str(&written).unwrap();
    assert_eq!(parsed["status"], "attached");
}

#[test]
fn missing_attachment_never_submits() {
    for response in [json!([]), json!({}), json!({"session_id": SESSION_ID})] {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(tmp.path());
        let mut rpc = FakeRpc::failing("session.create", Failure::Response(response));
        let err = create_and_submit_once(&mut rpc, &store, &make_request(tmp.path())).unwrap_err();
        assert!(matches!(
            err,
            DisposableAdmissionError::Session(ref inner) if inner.is_unknown()
        ));
        assert_eq!(rpc.methods(), ["session.create"]);
        assert!(store.get("submit-1").unwrap().is_none());
    }
}

#[test]
fn disconnected_creation_never_submits_or_retries() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let request = make_request(tmp.path());
    let mut rpc = FakeRpc::failing("session.create", Failure::Error("disconnect"));

    let err = create_and_submit_once(&mut rpc, &store, &request).unwrap_err();
    assert!(matches!(
        err,
        DisposableAdmissionError::Session(ref inner) if inner.is_unknown()
    ));
    let err = create_and_submit_once(&mut rpc, &store, &request).unwrap_err();
    assert!(matches!(
        err,
        DisposableAdmissionError::Session(SessionCreationError::Creation(_))
    ));
    assert!(err.to_string().contains("already exists"));
    assert_eq!(rpc.methods(), ["session.create"]);
}

#[test]
fn disconnected_submit_remains_unknown_and_cannot_repeat() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let request = make_request(tmp.path());
    let mut rpc = FakeRpc::failing("prompt.submit", Failure::Error("disconnect"));

    let err = create_and_submit_once(&mut rpc, &store, &request).unwrap_err();
    assert!(matches!(
        err,
        DisposableAdmissionError::Admission(LiveAdmissionError::Identity(
            GatewayIdentityError::OutcomeUnknown { .. }
        ))
    ));
    let record = store.get("submit-1").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    let err = create_and_submit_once(&mut rpc, &store, &request).unwrap_err();
    assert!(matches!(err, DisposableAdmissionError::Value(_)));
    assert!(err.to_string().contains("already exists"));
    assert_eq!(rpc.methods(), ["session.create", "prompt.submit"]);
}

#[test]
fn receipt_failure_preserves_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let mut rpc = FakeRpc::failing("prompt.receipt", Failure::Error("disconnect"));

    let err = create_and_submit_once(&mut rpc, &store, &make_request(tmp.path())).unwrap_err();
    assert!(matches!(
        err,
        DisposableAdmissionError::Admission(LiveAdmissionError::Rpc(_))
    ));
    let record = store.get("submit-1").unwrap().unwrap();
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
    assert_eq!(
        rpc.methods(),
        ["session.create", "prompt.submit", "prompt.receipt"]
    );
}

#[test]
fn invalid_submission_id_prevents_creation() {
    for submission_id in ["", " "] {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(tmp.path());
        let mut rpc = FakeRpc::default();
        let mut request = make_request(tmp.path());
        submission_id.clone_into(&mut request.submission_id);
        let err = create_and_submit_once(&mut rpc, &store, &request).unwrap_err();
        assert!(err.to_string().contains("submission_id"));
        assert_eq!(rpc.calls.len(), 0);
        assert!(!request.creation.record_path.exists());
    }
}

#[test]
fn existing_ledger_identity_prevents_creation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let request = make_request(tmp.path());
    let mut rpc = FakeRpc::default();
    create_and_submit_once(&mut rpc, &store, &request).unwrap();
    let mut other = FakeRpc::default();
    let err = create_and_submit_once(&mut other, &store, &request).unwrap_err();
    assert!(matches!(err, DisposableAdmissionError::Value(_)));
    assert!(err.to_string().contains("already exists"));
    assert_eq!(other.calls.len(), 0);
}
