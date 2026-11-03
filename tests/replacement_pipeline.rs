#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Local reviewed-replacement pipeline authorization and review tests.

use std::cell::RefCell;
use std::path::Path;
use std::process::Command;

use hermes_harness::multiagent::local_inference::{LocalEndpoint, LocalInferenceError, Transport};
use hermes_harness::multiagent::model_review::Verdict;
use hermes_harness::multiagent::replacement_pipeline::{
    ReplacementPipelineError, ReplacementReview, ReplacementTask, apply_reviewed_replacements,
    generate_reviewed_replacements,
};
use hermes_harness::util::ordered_map::OrderedMap;
use hermes_harness::util::sha256_hex;
use serde_json::{Value, json};

struct FakeTransport {
    digest: String,
    verdict: &'static str,
    calls: RefCell<Vec<String>>,
    expect_objective: Option<&'static str>,
}

impl FakeTransport {
    fn new(digest: &str, verdict: &'static str) -> Self {
        Self {
            digest: digest.to_owned(),
            verdict,
            calls: RefCell::new(Vec::new()),
            expect_objective: None,
        }
    }
}

impl Transport for FakeTransport {
    fn complete(
        &self,
        endpoint: &LocalEndpoint,
        payload: &[u8],
    ) -> Result<Vec<u8>, LocalInferenceError> {
        if let (Some(objective), "reviewer") = (self.expect_objective, endpoint.model.as_str()) {
            let data: Value = serde_json::from_slice(payload).unwrap();
            let messages = &data["messages"];
            let review: Value =
                serde_json::from_str(messages[1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(review["objective"], objective);
            assert!(
                messages[0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("unrelated")
            );
        }
        self.calls.borrow_mut().push(endpoint.model.clone());
        let text = if endpoint.model == "worker" {
            json!({
                "task_id": "t",
                "replacements": [{
                    "path": "src/a.py",
                    "expected_sha256": self.digest,
                    "content": "new = 2\n",
                }],
            })
            .to_string()
        } else {
            json!({"verdict": self.verdict, "findings": ["checked"]}).to_string()
        };
        Ok(serde_json::to_vec(&json!({"choices": [{"message": {"content": text}}]})).unwrap())
    }
}

fn endpoint(name: &str) -> LocalEndpoint {
    LocalEndpoint::new("http://127.0.0.1:11434/v1/chat/completions", name)
}

fn scopes() -> Vec<String> {
    vec!["src".to_owned()]
}

fn generate(
    transport: &FakeTransport,
    worker: &str,
    objective: &str,
    digest: &str,
) -> Result<ReplacementReview, ReplacementPipelineError> {
    let preimages: OrderedMap<String> = [("src/a.py".to_owned(), digest.to_owned())]
        .into_iter()
        .collect();
    let owned = scopes();
    generate_reviewed_replacements(
        &endpoint(worker),
        &endpoint("reviewer"),
        &ReplacementTask {
            task_id: "t",
            objective,
            owned_paths: &owned,
            preimages: &preimages,
        },
        transport,
        true,
    )
}

fn run(verdict: &'static str) -> (ReplacementReview, FakeTransport) {
    let digest = sha256_hex(b"old = 1\n");
    let transport = FakeTransport::new(&digest, verdict);
    let result = generate(&transport, "worker", "Replace", &digest).unwrap();
    (result, transport)
}

#[test]
fn serial_review_admission() {
    let (result, transport) = run("accept");
    assert_eq!(*transport.calls.borrow(), ["worker", "reviewer"]);
    assert_eq!(result.verdict, Verdict::Accept);
    assert!(!result.applied);
}

#[test]
fn rejected_review_blocks_application() {
    let tmp = tempfile::tempdir().unwrap();
    let (result, _) = run("changes_required");
    let error =
        apply_reviewed_replacements(&result, tmp.path(), &scopes(), "unused", true).unwrap_err();
    assert!(matches!(error, ReplacementPipelineError::Workflow(_)));
    assert!(error.to_string().contains("rejected"));
}

#[test]
fn separate_application_authorization() {
    let tmp = tempfile::tempdir().unwrap();
    let (result, _) = run("accept");
    let error =
        apply_reviewed_replacements(&result, tmp.path(), &scopes(), "unused", false).unwrap_err();
    assert!(matches!(error, ReplacementPipelineError::Workflow(_)));
    assert!(error.to_string().contains("separate authorization"));
}

#[test]
fn same_model_rejected_before_inference() {
    let digest = sha256_hex(b"old");
    let transport = FakeTransport::new(&digest, "accept");
    let error = generate(&transport, "reviewer", "Replace", &digest).unwrap_err();
    assert!(matches!(error, ReplacementPipelineError::Inference(_)));
    assert!(error.to_string().contains("distinct"));
    assert!(transport.calls.borrow().is_empty());
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn reviewed_replacement_applies_only_in_disposable_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.py"), "old = 1\n").unwrap();
    git(&repo, &["add", "src/a.py"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "baseline",
        ],
    );
    let workspace = tmp.path().join("worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "harness/replacement-test",
            workspace.to_str().unwrap(),
            "HEAD",
        ],
    );
    let baseline = git(&workspace, &["rev-parse", "HEAD"]);
    let (result, _) = run("accept");
    let applied =
        apply_reviewed_replacements(&result, &workspace, &scopes(), &baseline, true).unwrap();
    assert!(applied.applied);
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/a.py")).unwrap(),
        "new = 2\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("src/a.py")).unwrap(),
        "old = 1\n"
    );
}

#[test]
fn replacement_reviewer_receives_original_objective() {
    let objective = "Fix only the incorrect addition function";
    let digest = sha256_hex(b"old = 1\n");
    let mut transport = FakeTransport::new(&digest, "accept");
    transport.expect_objective = Some(objective);
    let result = generate(&transport, "worker", objective, &digest).unwrap();
    assert_eq!(result.verdict, Verdict::Accept);
    assert_eq!(*transport.calls.borrow(), ["worker", "reviewer"]);
}
