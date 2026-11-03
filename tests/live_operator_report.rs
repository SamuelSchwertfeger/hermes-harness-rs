#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Read-only cross-ledger inspector integration with actual disposable `SQLite` files.

use std::fs;
use std::path::Path;

use hermes_harness::operator::live_operator_report::{LedgerPaths, inspect_task};
use rusqlite::Connection;

fn ledgers(dir: &Path) -> LedgerPaths {
    let paths = LedgerPaths {
        bindings: dir.join("bindings.sqlite"),
        services: dir.join("services.sqlite"),
        admissions: dir.join("admissions.sqlite"),
    };
    let db = Connection::open(&paths.bindings).unwrap();
    db.execute_batch(
        "CREATE TABLE bindings (task_id TEXT, role TEXT, submission_id TEXT, session_id TEXT, unit TEXT);
         INSERT INTO bindings VALUES ('task-1','worker','sub-worker','session-worker','hermes-job-worker');
         INSERT INTO bindings VALUES ('task-1','reviewer','sub-reviewer','session-reviewer','hermes-job-reviewer');",
    )
    .unwrap();
    let db = Connection::open(&paths.services).unwrap();
    db.execute_batch(
        "CREATE TABLE services (unit TEXT, role TEXT, state TEXT);
         INSERT INTO services VALUES ('hermes-job-worker','worker','running_observed');
         INSERT INTO services VALUES ('hermes-job-reviewer','reviewer','stop_uncertain');",
    )
    .unwrap();
    let db = Connection::open(&paths.admissions).unwrap();
    db.execute_batch(
        "CREATE TABLE admissions (submission_id TEXT, session_id TEXT, admission_id TEXT, local_state TEXT);
         INSERT INTO admissions VALUES ('sub-worker','session-worker','a-worker','receipt_observed');
         INSERT INTO admissions VALUES ('sub-reviewer','session-reviewer',NULL,'outcome_unknown');",
    )
    .unwrap();
    paths
}

fn has(concerns: &[String], name: &str) -> bool {
    concerns.iter().any(|c| c == name)
}

fn message<T: std::fmt::Debug>(
    result: Result<T, hermes_harness::operator::live_operator_report::LiveReportError>,
) -> String {
    result.unwrap_err().to_string()
}

#[test]
fn joined_report_remains_non_authoritative() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    let report = inspect_task(&paths, "task-1").unwrap();
    let (worker, reviewer) = (&report.roles[0], &report.roles[1]);
    assert_eq!(worker.admission_id.as_deref(), Some("a-worker"));
    assert_eq!(worker.concerns, ["live_process_not_verified"]);
    assert!(has(&reviewer.concerns, "service_not_recorded_running"));
    assert!(has(&reviewer.concerns, "submission_uncertain_or_pending"));
    assert!(!report.safe_to_dispatch);
    assert!(!report.safe_to_retry);
    assert!(!report.approved);
    let parsed: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
    assert_eq!(parsed["task_id"], "task-1");
}

#[test]
fn inspection_never_changes_database() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    let read =
        || [&paths.bindings, &paths.services, &paths.admissions].map(|p| fs::read(p).unwrap());
    let before = read();
    inspect_task(&paths, "task-1").unwrap();
    inspect_task(&paths, "task-1").unwrap();
    assert_eq!(before, read());
}

#[test]
fn unknown_task_does_not_infer_previous_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let report = inspect_task(&ledgers(tmp.path()), "other-task").unwrap();
    assert!(
        report
            .roles
            .iter()
            .all(|r| has(&r.concerns, "binding_missing"))
    );
    assert!(report.roles.iter().all(|r| r.submission_id.is_none()));
}

#[test]
fn mismatched_session_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    Connection::open(&paths.admissions)
        .unwrap()
        .execute(
            "UPDATE admissions SET session_id='tampered' WHERE submission_id='sub-worker'",
            [],
        )
        .unwrap();
    let report = inspect_task(&paths, "task-1").unwrap();
    assert!(has(&report.roles[0].concerns, "session_mismatch"));
}

#[test]
fn missing_service_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    Connection::open(&paths.services)
        .unwrap()
        .execute("DELETE FROM services WHERE unit='hermes-job-worker'", [])
        .unwrap();
    let report = inspect_task(&paths, "task-1").unwrap();
    assert!(has(&report.roles[0].concerns, "service_missing"));
}

#[test]
fn service_role_tamper_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    Connection::open(&paths.services)
        .unwrap()
        .execute(
            "UPDATE services SET role='reviewer' WHERE unit='hermes-job-worker'",
            [],
        )
        .unwrap();
    let report = inspect_task(&paths, "task-1").unwrap();
    assert!(has(&report.roles[0].concerns, "service_role_mismatch"));
}

#[test]
fn missing_database_refused_without_creation() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    fs::remove_file(&paths.admissions).unwrap();
    assert!(message(inspect_task(&paths, "task-1")).contains("existing regular"));
    assert!(!paths.admissions.exists());
}

#[cfg(unix)]
#[test]
fn symlinked_database_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    let link = tmp.path().join("linked.sqlite");
    std::os::unix::fs::symlink(&paths.bindings, &link).unwrap();
    let linked = LedgerPaths {
        bindings: link,
        ..paths
    };
    assert!(message(inspect_task(&linked, "task-1")).contains("non-symlink"));
}

#[cfg(unix)]
#[test]
fn symlinked_parent_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    let linked_dir = tmp.path().join("linked-dir");
    std::os::unix::fs::symlink(tmp.path(), &linked_dir).unwrap();
    let linked = LedgerPaths {
        bindings: linked_dir.join("bindings.sqlite"),
        ..paths
    };
    assert!(message(inspect_task(&linked, "task-1")).contains("parent"));
}

#[test]
fn invalid_schema_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    fs::write(&paths.bindings, "not sqlite").unwrap();
    assert!(message(inspect_task(&paths, "task-1")).contains("schema"));
}

#[test]
fn duplicate_role_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = ledgers(tmp.path());
    Connection::open(&paths.bindings)
        .unwrap()
        .execute(
            "INSERT INTO bindings VALUES ('task-1','worker','sub-extra','session-extra','hermes-job-extra')",
            [],
        )
        .unwrap();
    assert!(message(inspect_task(&paths, "task-1")).contains("task-role"));
}

#[test]
fn blank_task_refused() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(message(inspect_task(&ledgers(tmp.path()), " ")).contains("Nonblank"));
}
