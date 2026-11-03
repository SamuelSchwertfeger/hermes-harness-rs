#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;

use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_identity::{GatewayIdentityError, Submission};
use hermes_harness::gateway::gateway_live_admission::{
    LiveAdmissionError, LiveAdmissionRequest, submit_once,
};
use hermes_harness::gateway::gateway_session_creation::{RpcFailure, SessionRpc};
use serde_json::{Map, Value, json};

/// Scripted response or injected failure for one RPC method.
#[derive(Clone)]
enum Scripted {
    Default,
    Response(Value),
    Failure(&'static str),
}

/// Scripted RPC that records every call and can inject a fault.
struct FakeRpc<'a> {
    store: &'a AdmissionStore,
    submission_id: String,
    submit_result: Scripted,
    receipt_result: Scripted,
    calls: Vec<(String, Map<String, Value>)>,
}

impl<'a> FakeRpc<'a> {
    fn new(store: &'a AdmissionStore, submission_id: &str) -> Self {
        Self {
            store,
            submission_id: submission_id.to_owned(),
            submit_result: Scripted::Default,
            receipt_result: Scripted::Default,
            calls: Vec::new(),
        }
    }

    fn methods(&self) -> Vec<&str> {
        self.calls
            .iter()
            .map(|(method, _)| method.as_str())
            .collect()
    }
}

fn resolve(result: &Scripted) -> Result<Value, RpcFailure> {
    match result {
        Scripted::Failure(text) => Err(std::io::Error::other(*text).into()),
        Scripted::Response(value) => Ok(value.clone()),
        Scripted::Default => Ok(json!({"admission_id": "adm-1", "status": "queued"})),
    }
}

impl SessionRpc for FakeRpc<'_> {
    fn rpc(&mut self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcFailure> {
        self.calls.push((method.to_owned(), params.clone()));
        let record = self.store.get(&self.submission_id).unwrap().unwrap();
        match method {
            "prompt.submit" => {
                assert_eq!(record.local_state, LocalState::OutcomeUnknown);
                resolve(&self.submit_result)
            }
            "prompt.receipt" => {
                assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
                resolve(&self.receipt_result)
            }
            _ => panic!("Unexpected RPC method: {method}"),
        }
    }
}

fn make_store(tmp: &Path) -> AdmissionStore {
    AdmissionStore::new(&tmp.join("admissions.sqlite")).unwrap()
}

fn make_submission() -> Submission {
    Submission::new("isolated-test-session", "probe-001")
}

fn request(submission: &Submission, text: &str) -> LiveAdmissionRequest {
    LiveAdmissionRequest {
        submission: submission.clone(),
        text: text.to_owned(),
    }
}

#[test]
fn submit_once_persists_identity_before_receipt_lookup() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let submission = make_submission();
    let mut rpc = FakeRpc::new(&store, &submission.submission_id);

    let result = submit_once(
        &store,
        &request(&submission, "Reply with M3_OK only; do not use tools."),
        &mut rpc,
    )
    .unwrap();

    assert_eq!(rpc.methods(), ["prompt.submit", "prompt.receipt"]);
    assert_eq!(result.admission_id, "adm-1");
    assert_eq!(result.submit_status, "queued");
    let record = store.get(&submission.submission_id).unwrap().unwrap();
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
    assert_eq!(record.local_state, LocalState::Admitted);
}

#[test]
fn submit_params_are_minimal_and_stable() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let submission = make_submission();
    let mut rpc = FakeRpc::new(&store, &submission.submission_id);

    submit_once(&store, &request(&submission, "M3 probe"), &mut rpc).unwrap();

    let (method, params) = &rpc.calls[0];
    assert_eq!(method, "prompt.submit");
    let expected = json!({
        "session_id": submission.session_id,
        "submission_id": submission.submission_id,
        "text": "M3 probe",
    });
    assert_eq!(Value::Object(params.clone()), expected);
    let expected_receipt = json!({
        "session_id": submission.session_id,
        "admission_id": "adm-1",
    });
    assert_eq!(Value::Object(rpc.calls[1].1.clone()), expected_receipt);
}

#[test]
fn submit_transport_failure_stays_unknown_and_never_retries() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let submission = make_submission();
    let mut rpc = FakeRpc::new(&store, &submission.submission_id);
    rpc.submit_result = Scripted::Failure("disconnected");

    let err = submit_once(&store, &request(&submission, "M3 probe"), &mut rpc).unwrap_err();
    assert!(matches!(
        err,
        LiveAdmissionError::Identity(GatewayIdentityError::OutcomeUnknown { .. })
    ));

    let record = store.get(&submission.submission_id).unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(record.admission_id, None);
    assert_eq!(rpc.methods(), ["prompt.submit"]);
}

#[test]
fn malformed_submit_response_stays_unknown_and_never_retries() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let submission = make_submission();
    let mut rpc = FakeRpc::new(&store, &submission.submission_id);
    rpc.submit_result = Scripted::Response(json!({"status": "queued"}));

    let err = submit_once(&store, &request(&submission, "M3 probe"), &mut rpc).unwrap_err();
    assert!(matches!(
        err,
        LiveAdmissionError::Identity(GatewayIdentityError::OutcomeUnknown { .. })
    ));

    let record = store.get(&submission.submission_id).unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(rpc.methods(), ["prompt.submit"]);
}

#[test]
fn receipt_read_failure_preserves_known_admission_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let submission = make_submission();
    let mut rpc = FakeRpc::new(&store, &submission.submission_id);
    rpc.receipt_result = Scripted::Failure("receipt disconnected");

    let err = submit_once(&store, &request(&submission, "M3 probe"), &mut rpc).unwrap_err();
    assert!(err.to_string().contains("receipt disconnected"));

    let record = store.get(&submission.submission_id).unwrap().unwrap();
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
    assert_eq!(record.local_state, LocalState::Admitted);
    assert_eq!(rpc.methods(), ["prompt.submit", "prompt.receipt"]);
}

#[test]
fn mismatched_receipt_cannot_change_known_admission_id() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let submission = make_submission();
    let mut rpc = FakeRpc::new(&store, &submission.submission_id);
    rpc.receipt_result =
        Scripted::Response(json!({"admission_id": "adm-other", "status": "queued"}));

    let err = submit_once(&store, &request(&submission, "M3 probe"), &mut rpc).unwrap_err();
    assert!(matches!(
        err,
        LiveAdmissionError::Identity(GatewayIdentityError::Contract(_))
    ));
    assert!(err.to_string().contains("changed admission identity"));

    let record = store.get(&submission.submission_id).unwrap().unwrap();
    assert_eq!(record.admission_id.as_deref(), Some("adm-1"));
    assert_eq!(record.local_state, LocalState::Admitted);
    assert_eq!(rpc.methods(), ["prompt.submit", "prompt.receipt"]);
}

#[test]
fn duplicate_submission_id_is_refused_before_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let submission = make_submission();
    store.prepare(&submission).unwrap();
    let mut rpc = FakeRpc::new(&store, &submission.submission_id);

    let err = submit_once(&store, &request(&submission, "M3 probe"), &mut rpc).unwrap_err();
    assert!(matches!(err, LiveAdmissionError::Store(_)));
    assert!(err.to_string().contains("already exists"));

    assert_eq!(rpc.calls.len(), 0);
}

#[test]
fn blank_probe_text_refused_before_ledger_or_rpc() {
    for text in ["", "   "] {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(tmp.path());
        let submission = make_submission();
        let mut rpc = FakeRpc::new(&store, &submission.submission_id);

        let err = submit_once(&store, &request(&submission, text), &mut rpc).unwrap_err();
        assert!(err.to_string().contains("nonblank"));

        assert!(store.get(&submission.submission_id).unwrap().is_none());
        assert_eq!(rpc.calls.len(), 0);
    }
}

#[test]
fn submission_with_existing_admission_id_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let submission = Submission::admitted("isolated-test-session", "probe-001", "adm-existing");
    let mut rpc = FakeRpc::new(&store, &submission.submission_id);

    let err = submit_once(&store, &request(&submission, "M3 probe"), &mut rpc).unwrap_err();
    assert!(err.to_string().contains("without an admission ID"));

    assert_eq!(rpc.calls.len(), 0);
}
