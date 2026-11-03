#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! One-shot bounded repair dispatch, interrupted recovery and trust boundaries.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hermes_harness::multiagent::local_inference::{LocalEndpoint, LocalInferenceError, Transport};
use hermes_harness::multiagent::model_review::Verdict;
use hermes_harness::multiagent::replace_edits::FileReplacement;
use hermes_harness::multiagent::replacement_pipeline::{
    ReplacementPipelineError, ReplacementReview, ReplacementTask,
};
use hermes_harness::multiagent::runtime::WorkflowError;
use hermes_harness::native::repair_controller::{
    DispositionRequest, open_repair, record_repair_disposition, reserve_repair,
};
use hermes_harness::native::repair_dispatch::{
    DispatchHost, DispatchRequest, SystemHost, dispatch_repair_with, inspect_dispatch,
};
use hermes_harness::util::{resolve_strict, sha256_hex};
use serde_json::{Map, Value, json};

struct NoTransport;

impl Transport for NoTransport {
    fn complete(&self, _: &LocalEndpoint, _: &[u8]) -> Result<Vec<u8>, LocalInferenceError> {
        panic!("the injected generator never reaches the transport")
    }
}

struct Host {
    audit: RefCell<BTreeMap<String, String>>,
    review: RefCell<Result<ReplacementReview, String>>,
    calls: Cell<usize>,
}

impl DispatchHost for Host {
    fn audit(
        &self,
        _root: &Path,
        _record: &Map<String, Value>,
    ) -> Result<BTreeMap<String, String>, WorkflowError> {
        Ok(self.audit.borrow().clone())
    }

    fn hostname(&self) -> String {
        "test-host".to_owned()
    }

    fn generate(
        &self,
        _worker: &LocalEndpoint,
        _reviewer: &LocalEndpoint,
        _task: &ReplacementTask<'_>,
        _transport: &dyn Transport,
    ) -> Result<ReplacementReview, ReplacementPipelineError> {
        self.calls.set(self.calls.get() + 1);
        match &*self.review.borrow() {
            Ok(review) => Ok(review.clone()),
            Err(message) => Err(WorkflowError::msg(message).into()),
        }
    }

    fn python_parses(&self, name: &str, content: &str) -> Result<bool, WorkflowError> {
        SystemHost.python_parses(name, content)
    }
}

fn write(path: &Path, value: &Value) -> String {
    let payload = format!("{}\n", serde_json::to_string(value).unwrap()).into_bytes();
    std::fs::write(path, &payload).unwrap();
    sha256_hex(&payload)
}

fn review_of(
    content: &str,
    digest: &str,
    verdict: Verdict,
    findings: &[&str],
) -> ReplacementReview {
    ReplacementReview {
        task_id: "task-1".to_owned(),
        replacements: vec![FileReplacement {
            path: "existing.py".to_owned(),
            expected_sha256: digest.to_owned(),
            content: content.to_owned(),
        }],
        reviewer_model: "qwen3:4b".to_owned(),
        verdict,
        findings: findings.iter().map(|item| (*item).to_owned()).collect(),
        applied: false,
    }
}

struct Fixture {
    state: PathBuf,
    checkpoint: PathBuf,
    digest: String,
    owned: Vec<String>,
    host: Host,
    _tmp: tempfile::TempDir,
}

fn setup() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = resolve_strict(tmp.path()).unwrap();
    let state = root.join("state");
    std::fs::create_dir(&state).unwrap();
    let workspace = root.join("worktree");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(workspace.join("existing.py"), b"print(0)\n").unwrap();
    let digest = sha256_hex(b"print(0)\n");
    let checkpoint = root.join("checkpoint.json");
    let workspace_text = workspace.to_string_lossy().into_owned();
    let checkpoint_hash = write(
        &checkpoint,
        &json!({
            "mode": "integrity-checkpoint",
            "task_id": "task-1",
            "baseline": "a".repeat(40),
            "workspace": workspace_text,
            "max_repair_attempts": 3,
            "files": [{"path": "existing.py", "sha256": digest}],
        }),
    );
    let failure = root.join("failed.json");
    write(
        &failure,
        &json!({
            "mode": "sandboxed-functional-verification",
            "task_id": "task-1",
            "baseline": "a".repeat(40),
            "workspace": workspace_text,
            "checkpoint_sha256": checkpoint_hash,
            "functional_verified": false,
            "tests_executed": true,
            "sandbox_started": true,
            "timed_out": false,
            "human_decision_pending": true,
        }),
    );
    open_repair(&failure, &checkpoint, &state, true).unwrap();
    reserve_repair(&state, "task-1", true).unwrap();
    let mut audit = BTreeMap::new();
    audit.insert("existing.py".to_owned(), digest.clone());
    let host = Host {
        audit: RefCell::new(audit),
        review: RefCell::new(Ok(review_of("print(1)\n", &digest, Verdict::Accept, &[]))),
        calls: Cell::new(0),
    };
    Fixture {
        state,
        checkpoint,
        digest,
        owned: vec!["existing.py".to_owned()],
        host,
        _tmp: tmp,
    }
}

fn dispatch<'a>(
    fixture: &'a Fixture,
    adjust: impl FnOnce(&mut DispatchRequest<'a>),
) -> Result<Value, hermes_harness::native::repair_dispatch::RepairDispatchError> {
    let mut request = DispatchRequest::new(
        &fixture.state,
        "task-1",
        &fixture.checkpoint,
        "Fix failed test",
        &fixture.owned,
    );
    request.authorized = true;
    request.transport = Some(&NoTransport);
    adjust(&mut request);
    dispatch_repair_with(&fixture.host, &request)
}

fn status(fixture: &Fixture) -> String {
    inspect_dispatch(&fixture.state, "task-1").unwrap()["status"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn reject(fixture: &Fixture, review: Option<&Path>) -> Result<Value, WorkflowError> {
    record_repair_disposition(&DispositionRequest {
        state: &fixture.state,
        task_id: "task-1",
        outcome: "rejected",
        review,
        verification: None,
        authorized: true,
    })
}

fn evidence_path(result: &Value) -> PathBuf {
    PathBuf::from(result["evidence"].as_str().unwrap())
}

fn expect<T: std::fmt::Debug, E: std::fmt::Display>(result: Result<T, E>, expected: &str) {
    let error = result.unwrap_err().to_string();
    assert!(error.contains(expected), "{error:?} lacks {expected:?}");
}

#[test]
fn accept_and_replay_fence() {
    let fixture = setup();
    let result = dispatch(&fixture, |_| {}).unwrap();
    assert_eq!(result["verdict"], json!("accept"));
    assert_eq!(result["applied"], json!(false));
    assert_eq!(result["generated_code_executed"], json!(false));
    assert_eq!(status(&fixture), "review-recorded");
    expect(dispatch(&fixture, |_| {}), "already dispatched");
    assert_eq!(fixture.host.calls.get(), 1);
}

#[test]
fn rejected_review_operator_disposition() {
    let fixture = setup();
    *fixture.host.review.borrow_mut() = Ok(review_of(
        "x = 1\n",
        &fixture.digest,
        Verdict::ChangesRequired,
        &["Incorrect logic"],
    ));
    let result = dispatch(&fixture, |_| {}).unwrap();
    let recorded = reject(&fixture, Some(&evidence_path(&result))).unwrap();
    assert_eq!(recorded["outcome"], json!("rejected"));
}

#[test]
fn interrupted_dispatch_does_not_repeat() {
    let fixture = setup();
    *fixture.host.review.borrow_mut() = Err("model transport interrupted".to_owned());
    expect(dispatch(&fixture, |_| {}), "interrupted");
    assert_eq!(status(&fixture), "dispatched-unresolved");
    expect(dispatch(&fixture, |_| {}), "already dispatched");
    assert_eq!(fixture.host.calls.get(), 1);
}

#[test]
fn dispatch_refuses_invalid_inputs() {
    for case in [
        "unowned",
        "wrong-model",
        "bad-preimage",
        "unauthorized",
        "stale",
    ] {
        let fixture = setup();
        match case {
            "unowned" => {
                let owned = vec!["not-owned.py".to_owned()];
                assert!(dispatch(&fixture, |request| request.owned_paths = &owned).is_err());
            }
            "wrong-model" => {
                let endpoint =
                    LocalEndpoint::new("http://127.0.0.1:11434/v1/chat/completions", "same");
                expect(
                    dispatch(&fixture, |request| {
                        request.worker = Some(&endpoint);
                        request.reviewer = Some(&endpoint);
                    }),
                    "differ",
                );
            }
            "bad-preimage" => {
                *fixture.host.review.borrow_mut() =
                    Ok(review_of("x\n", &"c".repeat(64), Verdict::Accept, &[]));
                expect(dispatch(&fixture, |_| {}), "preimage");
                assert_eq!(status(&fixture), "dispatched-unresolved");
            }
            "unauthorized" => {
                expect(
                    dispatch(&fixture, |request| request.authorized = false),
                    "authorization",
                );
            }
            _ => {
                fixture
                    .host
                    .audit
                    .borrow_mut()
                    .insert("existing.py".to_owned(), "c".repeat(64));
                expect(dispatch(&fixture, |_| {}), "drift");
            }
        }
        assert_eq!(
            fixture.host.calls.get(),
            usize::from(case == "bad-preimage"),
            "{case}"
        );
    }
}

#[test]
fn fake_review_cannot_settle_attempt() {
    let fixture = setup();
    let fake = fixture.state.join("fake.json");
    write(
        &fake,
        &json!({
            "mode": "repair-independent-review",
            "task_id": "task-1",
            "number": 1,
            "verdict": "changes_required",
            "worker_model": "worker",
            "reviewer_model": "reviewer",
        }),
    );
    expect(reject(&fixture, Some(&fake)), "native repair dispatch");
    assert_eq!(status(&fixture), "not-dispatched");
    dispatch(&fixture, |_| {}).unwrap();
}

#[test]
fn no_op_model_acceptance_is_policy_rejected() {
    let fixture = setup();
    *fixture.host.review.borrow_mut() = Ok(review_of(
        "print(0)\n",
        &fixture.digest,
        Verdict::Accept,
        &["Looks correct"],
    ));
    let result = dispatch(&fixture, |_| {}).unwrap();
    assert_eq!(result["model_verdict"], json!("accept"));
    assert_eq!(result["verdict"], json!("changes_required"));
    assert_eq!(result["policy_rejected_no_op"], json!(true));
    assert!(
        result["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding.as_str().unwrap().contains("No-op replacement"))
    );
    assert_eq!(result["applied"], json!(false));
    assert_eq!(fixture.host.calls.get(), 1);
    let recorded = reject(&fixture, Some(&evidence_path(&result))).unwrap();
    assert_eq!(recorded["outcome"], json!("rejected"));
}

#[test]
fn changed_source_not_rejected_as_no_op() {
    let fixture = setup();
    let result = dispatch(&fixture, |_| {}).unwrap();
    assert_eq!(result["verdict"], json!("accept"));
    assert_eq!(result["model_verdict"], json!("accept"));
    assert_eq!(result["policy_rejected_no_op"], json!(false));
    assert_eq!(result["policy_rejected_invalid_syntax"], json!(false));
}

#[test]
fn replay_preserves_evidence_hashes() {
    let fixture = setup();
    dispatch(&fixture, |_| {}).unwrap();
    let paths: Vec<PathBuf> = ["dispatch", "review"]
        .iter()
        .map(|suffix| fixture.state.join(format!("task-1-repair-1-{suffix}.json")))
        .collect();
    let digests = |paths: &[PathBuf]| -> Vec<String> {
        paths
            .iter()
            .map(|path| sha256_hex(&std::fs::read(path).unwrap()))
            .collect()
    };
    let before = digests(&paths);
    expect(dispatch(&fixture, |_| {}), "already dispatched");
    assert_eq!(digests(&paths), before);
    assert_eq!(fixture.host.calls.get(), 1);
}

#[test]
fn invalid_python_acceptance_is_policy_rejected() {
    let fixture = setup();
    *fixture.host.review.borrow_mut() = Ok(review_of(
        "print(1)\\n",
        &fixture.digest,
        Verdict::Accept,
        &["Looks correct"],
    ));
    let result = dispatch(&fixture, |_| {}).unwrap();
    assert_eq!(result["model_verdict"], json!("accept"));
    assert_eq!(result["verdict"], json!("changes_required"));
    assert_eq!(result["policy_rejected_invalid_syntax"], json!(true));
    assert!(
        result["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding.as_str().unwrap().contains("Invalid Python syntax"))
    );
    assert_eq!(fixture.host.calls.get(), 1);
}
