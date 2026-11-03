#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Durable task-to-service identity safety checks with real SQLite ledgers.

use hermes_harness::services::live_service_journal::{ServiceJournal, ServiceState};
use hermes_harness::supervised::live_execution_binding::{ExecutionBinding, ExecutionBindingStore};
use tempfile::TempDir;

fn context() -> (
    TempDir,
    ExecutionBindingStore,
    ServiceJournal,
    ExecutionBinding,
) {
    let tmp = tempfile::tempdir().unwrap();
    let store = ExecutionBindingStore::new(&tmp.path().join("bindings.sqlite")).unwrap();
    let journal = ServiceJournal::new(&tmp.path().join("services.sqlite")).unwrap();
    let binding = ExecutionBinding {
        task_id: "task-1".to_owned(),
        role: "worker".to_owned(),
        submission_id: "sub-1".to_owned(),
        session_id: "sess-1".to_owned(),
        unit: "hermes-job-task-1-worker".to_owned(),
    };
    (tmp, store, journal, binding)
}

#[test]
fn persist_and_reopen() {
    let (_tmp, store, journal, binding) = context();
    store.bind(&binding, &journal).unwrap();
    let reopened = ExecutionBindingStore::new(&store.path).unwrap();
    assert_eq!(
        reopened.get(&binding.task_id, "worker").unwrap(),
        Some(binding.clone())
    );
    let err = reopened.bind(&binding, &journal).unwrap_err();
    assert!(err.to_string().contains("Duplicate"));
}

#[test]
fn identity_cannot_be_reused() {
    for field in ["role", "submission_id", "session_id", "unit"] {
        let (_tmp, store, journal, binding) = context();
        store.bind(&binding, &journal).unwrap();
        let mut second = ExecutionBinding {
            task_id: "task-2".to_owned(),
            role: "reviewer".to_owned(),
            submission_id: "sub-2".to_owned(),
            session_id: "sess-2".to_owned(),
            unit: "hermes-job-task-2-reviewer".to_owned(),
        };
        if field == "role" {
            second.task_id.clone_from(&binding.task_id);
        }
        match field {
            "role" => second.role.clone_from(&binding.role),
            "submission_id" => second.submission_id.clone_from(&binding.submission_id),
            "session_id" => second.session_id.clone_from(&binding.session_id),
            _ => second.unit.clone_from(&binding.unit),
        }
        assert!(store.bind(&second, &journal).is_err(), "{field}");
    }
}

#[test]
fn running_requires_exact_record() {
    let (_tmp, store, journal, binding) = context();
    store.bind(&binding, &journal).unwrap();
    let err = store.require_running(&binding, &journal).unwrap_err();
    assert!(err.to_string().contains("not durably"));
    journal.reserve(&binding.unit, "worker", "{}").unwrap();
    let err = store.require_running(&binding, &journal).unwrap_err();
    assert!(err.to_string().contains("not durably"));
    journal
        .transition(
            &binding.unit,
            ServiceState::LaunchUncertain,
            ServiceState::RunningObserved,
        )
        .unwrap();
    store.require_running(&binding, &journal).unwrap();
    let tampered = ExecutionBinding {
        session_id: "tampered".to_owned(),
        ..binding
    };
    let err = store.require_running(&tampered, &journal).unwrap_err();
    assert!(err.to_string().contains("mismatch"));
}

#[cfg(unix)]
#[test]
fn symlink_store_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target");
    std::fs::write(&target, "not sqlite").unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let err = ExecutionBindingStore::new(&link).unwrap_err();
    assert!(err.to_string().contains("symlinks"));
}
