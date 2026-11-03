#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Hermes supervised planning tests never launch a gateway or process.

use std::path::Path;

use hermes_harness::agents::supervised_agents::{
    SupervisedAgentsError, SupervisedPolicy, prepare_supervised_plan,
};
use hermes_harness::core::delegation::{Brief, DelegationError};

fn brief() -> Brief {
    Brief {
        task_id: "test-1".to_owned(),
        objective: "Change one fixture".to_owned(),
        owned_paths: vec!["fixtures/example.txt".to_owned()],
        worker_id: "worker-a".to_owned(),
        reviewer_id: "reviewer-b".to_owned(),
    }
}

fn policy(path: &Path) -> SupervisedPolicy {
    SupervisedPolicy::new("test-1", path)
}

#[test]
fn distinct_disabled_invocations() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = prepare_supervised_plan(&brief(), &policy(tmp.path())).unwrap();
    assert_ne!(plan.worker.ticket_id, plan.reviewer.ticket_id);
    assert_ne!(plan.worker.prompt, plan.reviewer.prompt);
    let resolved = std::fs::canonicalize(tmp.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(plan.worker.workspace, resolved);
    assert_eq!(plan.reviewer.workspace, resolved);
    assert!(!plan.worker.execution_enabled);
    assert!(!plan.reviewer.execution_enabled);
    let err = plan.require_executable().unwrap_err();
    assert!(err.to_string().contains("execution disabled"));
}

#[test]
fn requires_explicit_task_authorization() {
    let tmp = tempfile::tempdir().unwrap();
    let policy = SupervisedPolicy::new("another", tmp.path());
    let err = prepare_supervised_plan(&brief(), &policy).unwrap_err();
    assert!(matches!(err, SupervisedAgentsError::Permission(_)));
    assert!(err.to_string().contains("Task-specific authorization"));
}

#[test]
fn rejects_same_role_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let mut same = brief();
    same.worker_id = "same".to_owned();
    same.reviewer_id = "same".to_owned();
    let err = prepare_supervised_plan(&same, &policy(tmp.path())).unwrap_err();
    assert!(matches!(
        err,
        SupervisedAgentsError::Delegation(DelegationError::Message(_))
    ));
    assert!(err.to_string().contains("identities must differ"));
}

#[test]
fn rejects_excessive_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let mut policy = policy(tmp.path());
    policy.deadline_seconds = 301;
    let err = prepare_supervised_plan(&brief(), &policy).unwrap_err();
    assert!(matches!(err, SupervisedAgentsError::Value(_)));
    assert!(err.to_string().contains("budgets"));
}

#[test]
fn rejects_missing_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let err = prepare_supervised_plan(&brief(), &policy(&tmp.path().join("missing"))).unwrap_err();
    assert!(matches!(err, SupervisedAgentsError::Value(_)));
    assert!(err.to_string().contains("workspace"));
}

#[cfg(unix)]
#[test]
fn rejects_symlink_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let err = prepare_supervised_plan(&brief(), &policy(&link)).unwrap_err();
    assert!(matches!(err, SupervisedAgentsError::Value(_)));
    assert!(err.to_string().contains("workspace"));
}
