#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Terminal receipt reconciliation never submits, retries, or lies about outcomes.

mod common;

use common::{FakeLedger, ScriptedReceipts};
use hermes_harness::gateway::gateway_identity::Receipt;
use hermes_harness::gateway::gateway_terminal_reconciliation::{
    TerminalReconciliationError, reconcile_terminal_once,
};
use serde_json::{Value, json};

const SESSION: &str = "local-example";
const SUBMISSION: &str = "submission-1";
const ADMISSION: &str = "admission-1";

fn store() -> FakeLedger {
    let db = FakeLedger::default();
    db.prepare(SESSION, SUBMISSION);
    db.record(
        SUBMISSION,
        &Receipt {
            admission_id: ADMISSION.to_owned(),
            status: "queued".to_owned(),
            execution_generation: None,
        },
    );
    db
}

fn receipt(outcome: &str) -> Value {
    json!({
        "admission_id": ADMISSION,
        "ref": {"session_id": SESSION, "profile_id": "/home/hermes/.hermes"},
        "status": "terminal",
        "outcome": outcome,
        "execution_generation": 1,
        "result": {"completed": outcome == "completed", "final_response": "M3_OK"},
    })
}

fn rpc(result: Option<Value>) -> ScriptedReceipts {
    let mut rpc = ScriptedReceipts::default();
    rpc.responses.insert(ADMISSION.to_owned(), result);
    rpc
}

fn with(base: Value, key: &str, value: Value) -> Value {
    let mut base = base;
    base[key] = value;
    base
}

#[test]
fn completed_result_is_durable() {
    let db = store();
    let mut rpc = rpc(Some(receipt("completed")));
    let result = reconcile_terminal_once(&mut rpc, &db, SUBMISSION).unwrap();
    assert_eq!(result.outcome, "completed");
    assert_eq!(result.final_response.as_deref(), Some("M3_OK"));
    assert_eq!(result.recorded.local_state, "receipt_observed");
    assert_eq!(result.recorded.gateway_status.as_deref(), Some("terminal"));
    assert_eq!(result.recorded.execution_generation, Some(1));
    assert_eq!(
        rpc.calls,
        vec![(
            "prompt.receipt".to_owned(),
            json!({"session_id": SESSION, "admission_id": ADMISSION, "include_result": true})
        )]
    );
}

#[test]
fn other_terminal_outcomes_are_not_success() {
    for outcome in ["failed", "interrupted", "cancelled"] {
        let db = store();
        let mut rpc = rpc(Some(receipt(outcome)));
        let evidence = reconcile_terminal_once(&mut rpc, &db, SUBMISSION).unwrap();
        assert_eq!(evidence.outcome, outcome);
        assert_eq!(
            evidence.recorded.gateway_status.as_deref(),
            Some("terminal")
        );
    }
}

#[test]
fn invalid_receipts_do_not_change_ledger() {
    let bad = [
        json!({"admission_id": ADMISSION, "status": "queued"}),
        json!({"admission_id": ADMISSION, "status": "terminal", "outcome": "completed"}),
        json!({"admission_id": ADMISSION, "status": "terminal", "outcome": "unknown"}),
        with(receipt("completed"), "admission_id", json!("wrong")),
        with(receipt("completed"), "ref", json!({"session_id": "wrong"})),
        with(receipt("completed"), "result", json!({"completed": false})),
    ];
    for response in bad {
        let db = store();
        let before = db.current(SUBMISSION);
        let mut rpc = rpc(Some(response));
        let err = reconcile_terminal_once(&mut rpc, &db, SUBMISSION).unwrap_err();
        assert!(matches!(
            err,
            TerminalReconciliationError::Contract(_)
                | TerminalReconciliationError::Reconciliation(_)
        ));
        assert_eq!(db.current(SUBMISSION), before);
        assert_eq!(rpc.methods(), vec!["prompt.receipt"]);
    }
}

#[test]
fn transport_failure_preserves_ledger() {
    let db = store();
    let before = db.current(SUBMISSION);
    let mut rpc = rpc(None);
    let err = reconcile_terminal_once(&mut rpc, &db, SUBMISSION).unwrap_err();
    assert!(matches!(err, TerminalReconciliationError::Rpc(_)));
    assert!(err.to_string().contains("unavailable"));
    assert_eq!(db.current(SUBMISSION), before);
    assert_eq!(rpc.calls.len(), 1);
}

#[test]
fn missing_admission_never_contacts_rpc() {
    let db = FakeLedger::default();
    let mut rpc = rpc(Some(receipt("completed")));
    let err = reconcile_terminal_once(&mut rpc, &db, SUBMISSION).unwrap_err();
    assert!(err.to_string().contains("Known durable"));
    assert!(rpc.calls.is_empty());
}
