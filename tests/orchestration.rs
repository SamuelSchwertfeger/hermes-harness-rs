#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! No live inference or subprocesses: deterministic control-plane contract tests.

use std::path::Path;

use hermes_harness::core::state::{Stage, create, save};
use hermes_harness::gateway::hermes_adapter::{PreparedInvocation, TaskKind};
use hermes_harness::supervised::orchestration::{
    DispatchError, TaskSpec, Worker, WorkerFailure, dispatch_offline,
};
use serde_json::Value;

struct FixedWorker;

impl Worker for FixedWorker {
    fn run(&self, _invocation: &PreparedInvocation) -> Result<String, WorkerFailure> {
        Ok("Verified offline response".to_owned())
    }
}

struct FailingWorker;

impl Worker for FailingWorker {
    fn run(&self, _invocation: &PreparedInvocation) -> Result<String, WorkerFailure> {
        Err(WorkerFailure {
            error_type: "RuntimeError".to_owned(),
            message: "Simulated failure".to_owned(),
        })
    }
}

fn spec(tmp: &Path, kind: TaskKind) -> TaskSpec {
    TaskSpec::new("offline-001", kind, "Review tests", tmp)
}

#[test]
fn scout_creates_private_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    create(tmp.path(), "offline-001", "Test").unwrap();
    let result =
        dispatch_offline(tmp.path(), &spec(tmp.path(), TaskKind::Scout), &FixedWorker).unwrap();
    let output = Path::new(&result.output_path);
    assert_eq!(
        std::fs::read_to_string(output).unwrap(),
        "Verified offline response"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(output).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let record: Value = serde_json::from_str(
        &std::fs::read_to_string(output.parent().unwrap().join("result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(record["model"], "granite4:3b");
    assert_eq!(record["status"], "succeeded");
}

#[test]
fn stage_mismatch_refuses_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    create(tmp.path(), "offline-001", "Test").unwrap();
    let err = dispatch_offline(
        tmp.path(),
        &spec(tmp.path(), TaskKind::Review),
        &FixedWorker,
    )
    .unwrap_err();
    assert!(matches!(err, DispatchError::Dispatch(_)));
    assert!(err.to_string().contains("not allowed"));
}

#[test]
fn cyber_rejected_before_any_worker_call() {
    let tmp = tempfile::tempdir().unwrap();
    create(tmp.path(), "offline-001", "Test").unwrap();
    let err =
        dispatch_offline(tmp.path(), &spec(tmp.path(), TaskKind::Cyber), &FixedWorker).unwrap_err();
    assert!(matches!(err, DispatchError::Dispatch(_)));
    assert!(err.to_string().contains("not authorized"));
}

#[test]
fn worker_failure_persists_sanitized_record() {
    let tmp = tempfile::tempdir().unwrap();
    create(tmp.path(), "offline-001", "Test").unwrap();
    let err = dispatch_offline(
        tmp.path(),
        &spec(tmp.path(), TaskKind::Scout),
        &FailingWorker,
    )
    .unwrap_err();
    assert!(err.to_string().contains("Simulated"));
    let runs = tmp.path().join("tickets/offline-001/runs");
    let records: Vec<_> = std::fs::read_dir(runs)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("failure.json"))
        .filter(|path| path.exists())
        .collect();
    assert_eq!(records.len(), 1);
    let text = std::fs::read_to_string(&records[0]).unwrap();
    let failure: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(failure["error_type"], "RuntimeError");
    assert!(!text.contains("Simulated failure"));
}

#[test]
fn plan_routed_to_codex_at_ticket_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ticket = create(tmp.path(), "offline-001", "Test").unwrap();
    ticket.stage = Stage::Ticket;
    save(tmp.path(), &ticket).unwrap();
    let result =
        dispatch_offline(tmp.path(), &spec(tmp.path(), TaskKind::Plan), &FixedWorker).unwrap();
    assert_eq!(
        (result.provider.as_str(), result.model.as_str()),
        ("openai-codex", "gpt-6.1-sol")
    );
}
