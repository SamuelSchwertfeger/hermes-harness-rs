#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline cancellation identity and uncertainty tests: no live Hermes activity.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;

use hermes_harness::gateway::gateway_identity::{
    GatewayIdentityError, OfflineAdmissionClient, Rpc, Submission,
};
use serde_json::{Value, json};

type Call = (String, BTreeMap<String, String>);

/// One-shot scripted transport that records all outbound method calls.
struct RecordingRpc {
    response: Value,
    failure: Option<io::ErrorKind>,
    calls: RefCell<Vec<Call>>,
}

impl RecordingRpc {
    fn with_response(response: Value) -> Self {
        Self {
            response,
            failure: None,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn with_failure(kind: io::ErrorKind) -> Self {
        Self {
            response: Value::Null,
            failure: Some(kind),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl Rpc for RecordingRpc {
    fn call(&self, method: &str, params: &BTreeMap<String, String>) -> Result<Value, io::Error> {
        self.calls
            .borrow_mut()
            .push((method.to_owned(), params.clone()));
        if let Some(kind) = self.failure {
            return Err(io::Error::new(kind, "fault"));
        }
        Ok(self.response.clone())
    }
}

fn cancel_params(session: &str, admission: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("session_id".to_owned(), session.to_owned()),
        ("admission_id".to_owned(), admission.to_owned()),
    ])
}

#[test]
fn cancel_targets_admission_identity_only() {
    let rpc = RecordingRpc::with_response(json!({"admission_id": "adm-a", "status": "queued"}));
    let receipt = OfflineAdmissionClient::new(&rpc)
        .cancel_queued(&Submission::admitted("session-a", "submission-a", "adm-a"))
        .unwrap();
    assert_eq!(receipt.admission_id, "adm-a");
    assert_eq!(receipt.status, "queued");
    assert_eq!(
        *rpc.calls.borrow(),
        vec![(
            "prompt.cancel".to_owned(),
            cancel_params("session-a", "adm-a")
        )]
    );
}

#[test]
fn cancel_status_is_opaque_not_terminal_proof() {
    for status in ["queued", "running", "complete", "cancelled", "unknown"] {
        let rpc = RecordingRpc::with_response(json!({"admission_id": "adm-a", "status": status}));
        let result = OfflineAdmissionClient::new(&rpc)
            .cancel_queued(&Submission::admitted("s", "sub", "adm-a"))
            .unwrap();
        assert_eq!(result.status, status);
        assert_eq!(result.admission_id, "adm-a");
        assert_eq!(rpc.calls.borrow().len(), 1);
    }
}

#[test]
fn cancel_bad_reply_cannot_be_accepted() {
    for response in [
        Value::Null,
        json!({}),
        json!({"admission_id": "sub", "status": "queued"}),
        json!({"admission_id": "other-admission", "status": "queued"}),
        json!({"admission_id": "adm-a"}),
        json!({"admission_id": "adm-a", "status": ""}),
        json!({"admission_id": "adm-a", "status": "queued", "execution_generation": true}),
        json!({"voice_stopped": true}),
    ] {
        let rpc = RecordingRpc::with_response(response);
        let error = OfflineAdmissionClient::new(&rpc)
            .cancel_queued(&Submission::admitted("s", "sub", "adm-a"))
            .unwrap_err();
        assert!(matches!(error, GatewayIdentityError::Contract(_)));
        assert_eq!(rpc.calls.borrow().len(), 1);
        assert_eq!(rpc.calls.borrow()[0].0, "prompt.cancel");
    }
}

#[test]
fn cancel_transport_failure_remains_ambiguous() {
    for kind in [io::ErrorKind::ConnectionReset, io::ErrorKind::TimedOut] {
        let rpc = RecordingRpc::with_failure(kind);
        let error = OfflineAdmissionClient::new(&rpc)
            .cancel_queued(&Submission::admitted("s", "sub", "adm-a"))
            .unwrap_err();
        assert!(matches!(error, GatewayIdentityError::OutcomeUnknown { .. }));
        assert!(error.to_string().contains("do not automatically retry"));
        assert_eq!(
            *rpc.calls.borrow(),
            vec![("prompt.cancel".to_owned(), cancel_params("s", "adm-a"))]
        );
    }
}

#[test]
fn cancel_without_admission_identity_never_calls_gateway() {
    for admission_id in [None, Some(""), Some("  ")] {
        let rpc = RecordingRpc::with_response(json!({"admission_id": "adm-a", "status": "queued"}));
        let mut submission = Submission::new("s", "sub");
        submission.admission_id = admission_id.map(str::to_owned);
        let error = OfflineAdmissionClient::new(&rpc)
            .cancel_queued(&submission)
            .unwrap_err();
        assert!(matches!(error, GatewayIdentityError::Contract(_)));
        assert!(error.to_string().contains("admission_id"));
        assert!(rpc.calls.borrow().is_empty());
    }
}

#[test]
fn cancel_result_does_not_mutate_caller_submission() {
    let submission = Submission::admitted("s", "sub", "adm-a");
    let rpc = RecordingRpc::with_response(json!({"admission_id": "adm-a", "status": "running"}));
    OfflineAdmissionClient::new(&rpc)
        .cancel_queued(&submission)
        .unwrap();
    assert_eq!(submission, Submission::admitted("s", "sub", "adm-a"));
    assert_eq!(rpc.calls.borrow().len(), 1);
}
