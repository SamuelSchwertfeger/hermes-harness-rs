#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Negative and recovery tests for append-only native repair admission.

use std::path::{Path, PathBuf};

use hermes_harness::native::repair_controller::{
    DispositionRequest, inspect_repair, open_repair, record_repair_disposition, reserve_repair,
};
use hermes_harness::util::sha256_hex;
use serde_json::{Value, json};

const EXPECTED_REMAINING: u64 = 2;
const EXPECTED_ATTEMPTS: u64 = 3;

fn receipt(path: &Path, record: &Value) -> String {
    let payload = format!("{}\n", serde_json::to_string(record).unwrap()).into_bytes();
    std::fs::write(path, &payload).unwrap();
    sha256_hex(&payload)
}

fn fixture(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let state = tmp.join("state");
    std::fs::create_dir(&state).unwrap();
    let checkpoint = tmp.join("checkpoint.json");
    let workspace = tmp.join("worktree").to_string_lossy().into_owned();
    let digest = receipt(
        &checkpoint,
        &json!({
            "mode": "integrity-checkpoint",
            "task_id": "task-1",
            "baseline": "a".repeat(40),
            "workspace": workspace,
            "max_repair_attempts": 3,
        }),
    );
    let failure = tmp.join("failure.json");
    receipt(
        &failure,
        &json!({
            "mode": "sandboxed-functional-verification",
            "task_id": "task-1",
            "baseline": "a".repeat(40),
            "workspace": workspace,
            "checkpoint_sha256": digest,
            "functional_verified": false,
            "tests_executed": true,
            "timed_out": false,
            "sandbox_started": true,
            "human_decision_pending": true,
        }),
    );
    (state, checkpoint, failure)
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn disposition(
    state: &Path,
    outcome: &str,
    review: Option<&Path>,
    authorized: bool,
) -> Result<Value, hermes_harness::multiagent::runtime::WorkflowError> {
    record_repair_disposition(&DispositionRequest {
        state,
        task_id: "task-1",
        outcome,
        review,
        verification: None,
        authorized,
    })
}

#[test]
fn open_and_reserve_append_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, checkpoint, failure) = fixture(tmp.path());
    let opened = open_repair(&failure, &checkpoint, &state, true).unwrap();
    assert_eq!(opened["automatic_execution_authorized"], json!(false));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(opened["evidence"].as_str().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let reserved = reserve_repair(&state, "task-1", true).unwrap();
    assert_eq!(reserved["number"], json!(1));
    assert_eq!(reserved["generated_code_executed"], json!(false));
    assert_eq!(
        inspect_repair(&state, "task-1").unwrap()["remaining"],
        json!(EXPECTED_REMAINING)
    );
    let error = reserve_repair(&state, "task-1", true).unwrap_err();
    assert!(error.to_string().contains("Prior attempt pending"));
    let error = open_repair(&failure, &checkpoint, &state, true).unwrap_err();
    assert!(error.is_already_exists());
}

#[test]
fn rejects_missing_authorization() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, checkpoint, failure) = fixture(tmp.path());
    let error = open_repair(&failure, &checkpoint, &state, false).unwrap_err();
    assert!(error.to_string().contains("authorization"));
    open_repair(&failure, &checkpoint, &state, true).unwrap();
    let error = reserve_repair(&state, "task-1", false).unwrap_err();
    assert!(error.to_string().contains("authorization"));
}

#[test]
fn refuses_unqualified_failures() {
    let cases = [
        ("tests_executed", json!(false)),
        ("sandbox_started", json!(false)),
        ("timed_out", json!(true)),
        ("functional_verified", json!(true)),
        ("checkpoint_sha256", json!("0".repeat(64))),
    ];
    for (field, value) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let (state, checkpoint, failure) = fixture(tmp.path());
        let mut record = read_json(&failure);
        record[field] = value;
        receipt(&failure, &record);
        assert!(open_repair(&failure, &checkpoint, &state, true).is_err());
        assert_eq!(std::fs::read_dir(&state).unwrap().count(), 0, "{field}");
    }
}

#[test]
fn refuses_modified_reservation() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, checkpoint, failure) = fixture(tmp.path());
    open_repair(&failure, &checkpoint, &state, true).unwrap();
    reserve_repair(&state, "task-1", true).unwrap();
    let path = state.join("task-1-repair-1.json");
    let mut record = read_json(&path);
    record["number"] = json!(2);
    receipt(&path, &record);
    let error = inspect_repair(&state, "task-1").unwrap_err();
    assert!(error.to_string().contains("Reservation mismatch"));
}

fn started(tmp: &Path) -> (PathBuf, Value) {
    let (state, checkpoint, failure) = fixture(tmp);
    open_repair(&failure, &checkpoint, &state, true).unwrap();
    let reservation = reserve_repair(&state, "task-1", true).unwrap();
    (state, reservation)
}

#[test]
fn abandoned_attempt_closes_sequence() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = started(tmp.path());
    let result = disposition(&state, "abandoned", None, true).unwrap();
    assert_eq!(result["next_stage"], json!("finished"));
    assert_eq!(
        inspect_repair(&state, "task-1").unwrap()["terminal"],
        json!(true)
    );
    let error = reserve_repair(&state, "task-1", true).unwrap_err();
    assert!(error.to_string().contains("pending or sequence closed"));
    let error = disposition(&state, "abandoned", None, true).unwrap_err();
    assert!(error.to_string().contains("No pending attempt"));
}

#[test]
fn rejected_attempt_requires_review() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = started(tmp.path());
    let error = disposition(&state, "rejected", None, true).unwrap_err();
    assert!(error.to_string().contains("Independent review"));
}

#[test]
fn three_rejections_exhaust_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, mut reservation) = started(tmp.path());
    let opened = read_json(&state.join("task-1-repair-open.json"));
    for number in 1..=EXPECTED_ATTEMPTS {
        let claim = json!({
            "mode": "repair-dispatch-claim",
            "task_id": "task-1",
            "number": number,
            "reservation_sha256": reservation["evidence_sha256"],
            "worker_model": "worker",
            "reviewer_model": "reviewer",
            "owned_paths": ["existing.py"],
            "preimages": {"existing.py": "b".repeat(64)},
        });
        let claim_hash = receipt(
            &state.join(format!("task-1-repair-{number}-dispatch.json")),
            &claim,
        );
        let path = state.join(format!("task-1-repair-{number}-review.json"));
        receipt(
            &path,
            &json!({
                "mode": "repair-independent-review",
                "task_id": "task-1",
                "number": number,
                "reservation_sha256": reservation["evidence_sha256"],
                "claim_sha256": claim_hash,
                "checkpoint_sha256": opened["checkpoint_sha256"],
                "source_failure_sha256": opened["failure_sha256"],
                "baseline": opened["baseline"],
                "workspace": opened["workspace"],
                "owned_paths": ["existing.py"],
                "worker_model": "worker",
                "reviewer_model": "reviewer",
                "verdict": "changes_required",
                "replacements": [{
                    "path": "existing.py",
                    "expected_sha256": "b".repeat(64),
                    "content": "x = 1\n",
                    "content_sha256": sha256_hex(b"x = 1\n"),
                }],
            }),
        );
        disposition(&state, "rejected", Some(&path), true).unwrap();
        if number < EXPECTED_ATTEMPTS {
            reservation = reserve_repair(&state, "task-1", true).unwrap();
        }
    }
    let result = inspect_repair(&state, "task-1").unwrap();
    assert_eq!(result["attempts_used"], json!(EXPECTED_ATTEMPTS));
    assert_eq!(result["next_stage"], json!("exhausted"));
    assert!(reserve_repair(&state, "task-1", true).is_err());
}

#[test]
fn attempt_disposition_requires_authorization() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, _) = started(tmp.path());
    let error = disposition(&state, "abandoned", None, false).unwrap_err();
    assert!(error.to_string().contains("authorization"));
    assert_eq!(
        inspect_repair(&state, "task-1").unwrap()["next_stage"],
        json!("await-attempt-disposition")
    );
}
