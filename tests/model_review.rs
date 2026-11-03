#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Independent inference-only reviewer contract tests.

use hermes_harness::multiagent::local_inference::{LocalEndpoint, LocalInferenceError, Transport};
use hermes_harness::multiagent::model_edits::{EditProposal, parse_edit_proposal};
use hermes_harness::multiagent::model_review::{Verdict, review_proposal};
use serde_json::{Value, json};

struct ReviewTransport {
    response: String,
    expect_objective: Option<&'static str>,
}

impl ReviewTransport {
    fn new(response: &str) -> Self {
        Self {
            response: response.to_owned(),
            expect_objective: None,
        }
    }
}

impl Transport for ReviewTransport {
    fn complete(
        &self,
        endpoint: &LocalEndpoint,
        payload: &[u8],
    ) -> Result<Vec<u8>, LocalInferenceError> {
        let data: Value = serde_json::from_slice(payload).unwrap();
        assert_eq!(data["model"], endpoint.model);
        assert_eq!(data["stream"], Value::Bool(false));
        if let Some(objective) = self.expect_objective {
            let messages = &data["messages"];
            let review_input: Value =
                serde_json::from_str(messages[1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(review_input["objective"], objective);
            assert!(
                messages[0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("unrelated")
            );
        }
        Ok(
            serde_json::to_vec(&json!({"choices": [{"message": {"content": self.response}}]}))
                .unwrap(),
        )
    }
}

fn proposal() -> EditProposal {
    let body = json!({
        "task_id": "t",
        "files": [{"path": "src/a.py", "content": "value = 1\n"}],
    })
    .to_string();
    parse_edit_proposal(&body, "t", &["src".to_owned()]).unwrap()
}

fn endpoint(model: &str) -> LocalEndpoint {
    LocalEndpoint::new("http://127.0.0.1:8080/v1/chat/completions", model)
}

const ACCEPT: &str = r#"{"verdict":"accept","findings":[]}"#;

#[test]
fn read_only_review_verdict() {
    let result = review_proposal(
        &proposal(),
        &endpoint("worker"),
        &endpoint("reviewer"),
        &ReviewTransport::new(ACCEPT),
        None,
    )
    .unwrap();
    assert_eq!(result.verdict, Verdict::Accept);
    assert!(!result.authorizes_merge);
    assert_eq!(result.reviewer_model, "reviewer");
}

#[test]
fn same_model_cannot_review_itself() {
    let error = review_proposal(
        &proposal(),
        &endpoint("same"),
        &endpoint("same"),
        &ReviewTransport::new(ACCEPT),
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("distinct"));
}

#[test]
fn rejects_unsupported_verdict() {
    let error = review_proposal(
        &proposal(),
        &endpoint("worker"),
        &endpoint("reviewer"),
        &ReviewTransport::new(r#"{"verdict":"approve_merge","findings":[]}"#),
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("Unsupported"));
}

#[test]
fn reviewer_receives_exact_task_scope() {
    let objective = "Implement only add(a, b) as a + b";
    let mut transport = ReviewTransport::new(ACCEPT);
    transport.expect_objective = Some(objective);
    let result = review_proposal(
        &proposal(),
        &endpoint("worker"),
        &endpoint("reviewer"),
        &transport,
        Some(objective),
    )
    .unwrap();
    assert_eq!(result.verdict, Verdict::Accept);
}

#[test]
fn verdict_from_str_accepts_only_supported_strings() {
    use hermes_harness::multiagent::model_review::Verdict;
    for verdict in [Verdict::Accept, Verdict::ChangesRequired] {
        assert_eq!(verdict.as_str().parse::<Verdict>().unwrap(), verdict);
    }
    for value in ["", "Accept", "changes-required"] {
        assert_eq!(
            value.parse::<Verdict>().unwrap_err().to_string(),
            "Unsupported review verdict"
        );
    }
}
