#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline recovery and safety invariants; no gateway or provider invoked.

use std::path::Path;

use hermes_harness::supervised::live_supervised_recovery::{
    Phase, RecoveryIdentity, RecoveryLedger, checkpoint_pending_human, evidence_digest,
    inspect_pending_decision,
};
use serde_json::{Value, json};

fn identity() -> RecoveryIdentity {
    RecoveryIdentity::new("task", "worker", "reviewer").unwrap()
}

fn ledger_path(dir: &Path) -> std::path::PathBuf {
    dir.join("state.db")
}

#[test]
fn restart_is_idempotent_but_no_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let identity = identity();
    let path = ledger_path(tmp.path());
    let ledger = RecoveryLedger::new(&path).unwrap();
    assert_eq!(ledger.initialize(&identity).unwrap().phase, Phase::Planned);
    let digest = evidence_digest(&json!({"worker_admission": "w-1"})).unwrap();
    let step = (Phase::Planned, Phase::WorkerAdmitted);
    assert_eq!(
        ledger.advance(&identity, step, &digest).unwrap().phase,
        Phase::WorkerAdmitted
    );
    let restarted = RecoveryLedger::new(&path).unwrap();
    assert_eq!(
        restarted.initialize(&identity).unwrap().phase,
        Phase::WorkerAdmitted
    );
    let err = restarted.advance(&identity, step, &digest).unwrap_err();
    assert!(err.to_string().contains("Stale checkpoint"));
}

#[test]
fn identity_collision_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let ledger = RecoveryLedger::new(&ledger_path(tmp.path())).unwrap();
    ledger.initialize(&identity()).unwrap();
    let other = RecoveryIdentity::new("task", "other", "reviewer").unwrap();
    let err = ledger.initialize(&other).unwrap_err();
    assert!(err.to_string().contains("identity"));
}

#[test]
fn halt_freezes_recovery() {
    for reason in ["operator denied", "cancelled", "gateway uncertain"] {
        let tmp = tempfile::tempdir().unwrap();
        let identity = identity();
        let ledger = RecoveryLedger::new(&ledger_path(tmp.path())).unwrap();
        ledger.initialize(&identity).unwrap();
        assert_eq!(ledger.halt(&identity, reason).unwrap().phase, Phase::Halted);
        let reread = RecoveryLedger::new(&ledger_path(tmp.path())).unwrap();
        assert_eq!(
            reread.read(&identity).unwrap().halted_reason.as_deref(),
            Some(reason)
        );
        let err = ledger
            .advance(
                &identity,
                (Phase::Planned, Phase::WorkerAdmitted),
                &"a".repeat(64),
            )
            .unwrap_err();
        assert!(err.to_string().contains("Stale checkpoint"));
    }
}

#[test]
fn recovery_rejects_skipping_phases() {
    let tmp = tempfile::tempdir().unwrap();
    let identity = identity();
    let ledger = RecoveryLedger::new(&ledger_path(tmp.path())).unwrap();
    ledger.initialize(&identity).unwrap();
    let err = ledger
        .advance(
            &identity,
            (Phase::Planned, Phase::ReviewerAdmitted),
            &"a".repeat(64),
        )
        .unwrap_err();
    assert!(err.to_string().contains("skip"));
}

#[test]
fn pending_decision_requires_unapproved_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let identity = identity();
    let path = tmp.path().join("human-decision-request.json");
    let mut record = json!({
        "task_id": "task",
        "worker_submission_id": "worker",
        "reviewer_submission_id": "reviewer",
        "human_decision": "pending",
        "approved": false,
    });
    std::fs::write(&path, record.to_string()).unwrap();
    assert_eq!(
        inspect_pending_decision(&path, &identity).unwrap().len(),
        64
    );
    record["approved"] = Value::Bool(true);
    std::fs::write(&path, record.to_string()).unwrap();
    let err = inspect_pending_decision(&path, &identity).unwrap_err();
    assert!(err.to_string().contains("not pending"));
}

#[test]
fn pending_checkpoint_never_approves() {
    let tmp = tempfile::tempdir().unwrap();
    let identity = identity();
    let ledger = RecoveryLedger::new(&ledger_path(tmp.path())).unwrap();
    ledger.initialize(&identity).unwrap();
    let steps = [
        (Phase::Planned, Phase::WorkerAdmitted),
        (Phase::WorkerAdmitted, Phase::WorkerAudited),
        (Phase::WorkerAudited, Phase::ReviewerAdmitted),
    ];
    for step in steps {
        ledger.advance(&identity, step, &"a".repeat(64)).unwrap();
    }
    let path = tmp.path().join("human-decision-request.json");
    let record = json!({
        "task_id": identity.task_id,
        "worker_submission_id": identity.worker_submission_id,
        "reviewer_submission_id": identity.reviewer_submission_id,
        "human_decision": "pending",
        "approved": false,
    });
    std::fs::write(&path, record.to_string()).unwrap();
    assert_eq!(
        checkpoint_pending_human(&ledger, &identity, &path)
            .unwrap()
            .phase,
        Phase::PendingHuman
    );
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(stored["approved"], Value::Bool(false));
    let err = checkpoint_pending_human(&ledger, &identity, &path).unwrap_err();
    assert!(err.to_string().contains("Stale checkpoint"));
}
