#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline identity contract tests; no gateway or model processes are started.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;

use hermes_harness::gateway::gateway_identity::{
    GatewayIdentityError, OfflineAdmissionClient, Rpc, Submission, parse_receipt,
};
use serde_json::{Value, json};

type Call = (String, BTreeMap<String, String>);

struct ScriptedRpc {
    calls: RefCell<Vec<Call>>,
    failure: Option<&'static str>,
    response: Value,
}

impl ScriptedRpc {
    fn new() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            failure: None,
            response: json!({"admission_id": "adm-1", "status": "queued"}),
        }
    }
}

impl Rpc for ScriptedRpc {
    fn call(&self, method: &str, params: &BTreeMap<String, String>) -> Result<Value, io::Error> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        if self.failure == Some(method) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "simulated disconnect",
            ));
        }
        Ok(self.response.clone())
    }
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn submission_keeps_both_identities() {
    let rpc = ScriptedRpc::new();
    let client = OfflineAdmissionClient::new(&rpc);
    let submission = Submission::new("session-1", "stable-submission");
    let (accepted, receipt) = client.submit(&submission, "safe task").unwrap();
    assert_eq!(accepted.submission_id, "stable-submission");
    assert_eq!(accepted.admission_id.as_deref(), Some("adm-1"));
    assert_eq!(receipt.status, "queued");
    assert_eq!(
        *rpc.calls.borrow(),
        vec![(
            "prompt.submit".to_owned(),
            map(&[
                ("session_id", "session-1"),
                ("submission_id", "stable-submission"),
                ("text", "safe task"),
            ])
        )]
    );
}

#[test]
fn receipt_uses_admission_id_not_submission_id() {
    let rpc = ScriptedRpc::new();
    let client = OfflineAdmissionClient::new(&rpc);
    client
        .receipt(&Submission::admitted("s", "submission-x", "adm-1"))
        .unwrap();
    assert_eq!(
        rpc.calls.borrow()[0],
        (
            "prompt.receipt".to_owned(),
            map(&[("session_id", "s"), ("admission_id", "adm-1")])
        )
    );
}

#[test]
fn cancel_is_not_a_terminal_assertion() {
    let rpc = ScriptedRpc::new();
    let receipt = OfflineAdmissionClient::new(&rpc)
        .cancel_queued(&Submission::admitted("s", "sub", "adm-1"))
        .unwrap();
    assert_eq!(receipt.status, "queued");
    assert_eq!(rpc.calls.borrow()[0].0, "prompt.cancel");
}

#[test]
fn identity_mismatch_is_rejected() {
    let mut rpc = ScriptedRpc::new();
    rpc.response = json!({"admission_id": "different", "status": "queued"});
    let error = OfflineAdmissionClient::new(&rpc)
        .receipt(&Submission::admitted("s", "sub", "adm-1"))
        .unwrap_err();
    assert!(matches!(error, GatewayIdentityError::Contract(_)));
    assert!(error.to_string().contains("changed admission identity"));
}

#[test]
fn disconnect_never_resubmits() {
    let mut rpc = ScriptedRpc::new();
    rpc.failure = Some("prompt.submit");
    let error = OfflineAdmissionClient::new(&rpc)
        .submit(&Submission::new("s", "same-id"), "hello")
        .unwrap_err();
    assert!(matches!(error, GatewayIdentityError::OutcomeUnknown { .. }));
    assert_eq!(
        error.to_string(),
        "prompt.submit outcome unknown; do not automatically retry"
    );
    let source = std::error::Error::source(&error).unwrap();
    assert!(source.downcast_ref::<io::Error>().is_some());
    assert!(format!("{:?}", OfflineAdmissionClient::new(&rpc)).contains("OfflineAdmissionClient"));
    assert_eq!(rpc.calls.borrow().len(), 1);
}

#[test]
fn rejects_malformed_and_voice_stop_responses() {
    for raw in [
        Value::Null,
        json!({"voice_stopped": true}),
        json!({"admission_id": "", "status": "queued"}),
        json!({"admission_id": "a", "status": ""}),
        json!({"admission_id": "a", "status": "queued", "execution_generation": true}),
    ] {
        let error = parse_receipt(&raw, None).unwrap_err();
        assert!(matches!(error, GatewayIdentityError::Contract(_)));
    }
}

#[test]
fn no_receipt_or_cancel_without_admission_identity() {
    let rpc = ScriptedRpc::new();
    let client = OfflineAdmissionClient::new(&rpc);
    let submission = Submission::new("s", "sub");
    assert!(matches!(
        client.receipt(&submission),
        Err(GatewayIdentityError::Contract(_))
    ));
    assert!(matches!(
        client.cancel_queued(&submission),
        Err(GatewayIdentityError::Contract(_))
    ));
}
