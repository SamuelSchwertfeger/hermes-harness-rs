#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Local worker/reviewer orchestration is serial and approval-gated.

use std::cell::RefCell;

use hermes_harness::multiagent::local_inference::{LocalEndpoint, LocalInferenceError, Transport};
use hermes_harness::multiagent::model_pipeline::{
    ModelPipelineError, apply_reviewed_new_files, generate_and_review,
};
use hermes_harness::multiagent::model_review::Verdict;
use serde_json::json;

struct SerialResponses {
    calls: RefCell<Vec<String>>,
    verdict: &'static str,
}

impl SerialResponses {
    fn new(verdict: &'static str) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            verdict,
        }
    }
}

impl Transport for SerialResponses {
    fn complete(
        &self,
        endpoint: &LocalEndpoint,
        _payload: &[u8],
    ) -> Result<Vec<u8>, LocalInferenceError> {
        self.calls.borrow_mut().push(endpoint.model.clone());
        let text = if endpoint.model == "coder" {
            json!({
                "task_id": "task-1",
                "files": [{"path": "tests/new.py", "content": "assert True\n"}],
            })
            .to_string()
        } else {
            json!({"verdict": self.verdict, "findings": []}).to_string()
        };
        Ok(serde_json::to_vec(&json!({"choices": [{"message": {"content": text}}]})).unwrap())
    }
}

fn endpoint(model: &str) -> LocalEndpoint {
    LocalEndpoint::new("http://127.0.0.1:11434/v1/chat/completions", model)
}

fn scopes() -> Vec<String> {
    vec!["tests".to_owned()]
}

fn run(transport: &SerialResponses, authorized: bool) -> Result<ModelOutcome, ModelPipelineError> {
    generate_and_review(
        "task-1",
        "Write test",
        &scopes(),
        &endpoint("coder"),
        &endpoint("reviewer"),
        transport,
        authorized,
    )
}

type ModelOutcome = hermes_harness::multiagent::model_pipeline::ProposalReviewResult;

#[test]
fn serial_model_proposal_and_review() {
    let transport = SerialResponses::new("accept");
    let outcome = run(&transport, true).unwrap();
    assert_eq!(*transport.calls.borrow(), ["coder", "reviewer"]);
    assert_eq!(outcome.review.verdict, Verdict::Accept);
    assert!(outcome.applied.is_none());
    assert!(!outcome.review.authorizes_merge);
}

#[test]
fn inference_requires_authorization() {
    let transport = SerialResponses::new("accept");
    let error = run(&transport, false).unwrap_err();
    assert!(matches!(error, ModelPipelineError::Workflow(_)));
    assert!(error.to_string().contains("authorization"));
    assert!(transport.calls.borrow().is_empty());
}

#[test]
fn refused_review_cannot_be_applied() {
    let tmp = tempfile::tempdir().unwrap();
    let outcome = run(&SerialResponses::new("changes_required"), true).unwrap();
    let error =
        apply_reviewed_new_files(&outcome, tmp.path(), &scopes(), "unused", true).unwrap_err();
    assert!(matches!(error, ModelPipelineError::Workflow(_)));
    assert!(error.to_string().contains("did not accept"));
    assert!(!tmp.path().join("tests").exists());
}

#[test]
fn application_needs_independent_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let outcome = run(&SerialResponses::new("accept"), true).unwrap();
    let error =
        apply_reviewed_new_files(&outcome, tmp.path(), &scopes(), "unused", false).unwrap_err();
    assert!(matches!(error, ModelPipelineError::Workflow(_)));
    assert!(error.to_string().contains("application"));
}
