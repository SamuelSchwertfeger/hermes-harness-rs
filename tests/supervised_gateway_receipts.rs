#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline gateway role receipt bridge integration tests.

mod common;

use common::{FakeLedger, ScriptedReceipts};
use hermes_harness::agents::supervised_agents::{
    SupervisedPlan, SupervisedPolicy, prepare_supervised_plan,
};
use hermes_harness::core::delegation::Brief;
use hermes_harness::gateway::gateway_identity::Receipt;
use hermes_harness::gateway::supervised_gateway_receipts::{
    ReceiptSources, RoleAdmissions, RoleReceiptError, reconcile_supervised_roles_once,
};
use serde_json::{Value, json};

fn receipt(session: &str, admission: &str, outcome: &str) -> Value {
    json!({
        "admission_id": admission,
        "ref": {"session_id": session},
        "status": "terminal",
        "outcome": outcome,
        "execution_generation": 1,
        "result": {"completed": outcome == "completed", "final_response": "offline test evidence"},
    })
}

fn setup(
    tmp: &tempfile::TempDir,
) -> (SupervisedPlan, RoleAdmissions, FakeLedger, ScriptedReceipts) {
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let brief = Brief {
        task_id: "task1".to_owned(),
        objective: "test".to_owned(),
        owned_paths: vec!["file.txt".to_owned()],
        worker_id: "worker".to_owned(),
        reviewer_id: "reviewer".to_owned(),
    };
    let plan = prepare_supervised_plan(&brief, &SupervisedPolicy::new("task1", workspace)).unwrap();
    let ids = RoleAdmissions {
        task_id: "task1".to_owned(),
        worker_submission_id: "sub-w".to_owned(),
        reviewer_submission_id: "sub-r".to_owned(),
    };
    let store = FakeLedger::default();
    for role in ["w", "r"] {
        store.prepare(&format!("session-{role}"), &format!("sub-{role}"));
        store.record(
            &format!("sub-{role}"),
            &Receipt {
                admission_id: format!("admission-{role}"),
                status: "queued".to_owned(),
                execution_generation: None,
            },
        );
    }
    let mut rpc = ScriptedReceipts::default();
    rpc.responses.insert(
        "admission-w".to_owned(),
        Some(receipt("session-w", "admission-w", "completed")),
    );
    rpc.responses.insert(
        "admission-r".to_owned(),
        Some(receipt("session-r", "admission-r", "completed")),
    );
    (plan, ids, store, rpc)
}

#[test]
fn two_distinct_successes_only_read_receipts() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, ids, store, mut rpc) = setup(&tmp);
    let sources = ReceiptSources {
        store: &store,
        rpc: &mut rpc,
    };
    let complete = reconcile_supervised_roles_once(&plan, &ids, sources).unwrap();
    assert_eq!(complete.worker.outcome, "completed");
    assert_eq!(complete.reviewer.outcome, "completed");
    assert_eq!(rpc.methods(), vec!["prompt.receipt", "prompt.receipt"]);
    let admissions: Vec<_> = rpc
        .calls
        .iter()
        .map(|(_, p)| p["admission_id"].clone())
        .collect();
    assert_eq!(admissions, vec![json!("admission-w"), json!("admission-r")]);
}

#[test]
fn rejects_wrong_task_without_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, mut ids, store, mut rpc) = setup(&tmp);
    ids.task_id = "other".to_owned();
    let sources = ReceiptSources {
        store: &store,
        rpc: &mut rpc,
    };
    let err = reconcile_supervised_roles_once(&plan, &ids, sources).unwrap_err();
    assert!(err.to_string().contains("authorized task"));
    assert!(rpc.calls.is_empty());
}

#[test]
fn rejects_duplicate_role_id_without_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, mut ids, store, mut rpc) = setup(&tmp);
    ids.reviewer_submission_id = "sub-w".to_owned();
    let sources = ReceiptSources {
        store: &store,
        rpc: &mut rpc,
    };
    let err = reconcile_supervised_roles_once(&plan, &ids, sources).unwrap_err();
    assert!(err.to_string().contains("distinct"));
    assert!(rpc.calls.is_empty());
}

#[test]
fn failed_worker_never_reads_reviewer() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, ids, store, mut rpc) = setup(&tmp);
    rpc.responses.insert(
        "admission-w".to_owned(),
        Some(receipt("session-w", "admission-w", "failed")),
    );
    let sources = ReceiptSources {
        store: &store,
        rpc: &mut rpc,
    };
    let err = reconcile_supervised_roles_once(&plan, &ids, sources).unwrap_err();
    assert!(matches!(err, RoleReceiptError::Receipt(_)));
    assert!(err.to_string().contains("Worker"));
    assert_eq!(rpc.methods(), vec!["prompt.receipt"]);
}

#[test]
fn unknown_worker_preserves_ledger_and_never_retries() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, ids, store, mut rpc) = setup(&tmp);
    let before = store.current("sub-w");
    rpc.responses.insert("admission-w".to_owned(), None);
    let sources = ReceiptSources {
        store: &store,
        rpc: &mut rpc,
    };
    let err = reconcile_supervised_roles_once(&plan, &ids, sources).unwrap_err();
    assert!(err.to_string().contains("unavailable"));
    assert_eq!(store.current("sub-w"), before);
    assert_eq!(rpc.calls.len(), 1);
}

#[test]
fn missing_admission_rejects_before_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, mut ids, store, mut rpc) = setup(&tmp);
    ids.worker_submission_id = "missing".to_owned();
    let sources = ReceiptSources {
        store: &store,
        rpc: &mut rpc,
    };
    let err = reconcile_supervised_roles_once(&plan, &ids, sources).unwrap_err();
    assert!(err.to_string().contains("preexisting"));
    assert!(rpc.calls.is_empty());
}
