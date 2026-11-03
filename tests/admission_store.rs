#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;

use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_identity::{Receipt, Submission};

fn make_store(tmp: &Path) -> AdmissionStore {
    AdmissionStore::new(&tmp.join("private").join("admissions.sqlite3")).unwrap()
}

fn receipt(id: &str, status: &str, generation: Option<u64>) -> Receipt {
    Receipt {
        admission_id: id.to_owned(),
        status: status.to_owned(),
        execution_generation: generation,
    }
}

#[test]
fn prepare_persists_before_network_io_and_uses_private_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let record = store
        .prepare(&Submission::new("session-a", "submission-a"))
        .unwrap();

    assert_eq!(record.local_state, LocalState::Prepared);
    assert_eq!(record.admission_id, None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(tmp.path().join("private").join("admissions.sqlite3"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(store.get("submission-a").unwrap(), Some(record));
}

#[test]
fn duplicate_submission_does_not_overwrite_record() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let original = store
        .prepare(&Submission::new("session-a", "submission-a"))
        .unwrap();

    let err = store
        .prepare(&Submission::new("session-b", "submission-a"))
        .unwrap_err();
    assert!(err.to_string().contains("already exists"));

    assert_eq!(store.get("submission-a").unwrap(), Some(original));
}

#[test]
fn unknown_outcome_survives_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("admissions.sqlite3");
    let store = AdmissionStore::new(&path).unwrap();
    store
        .prepare(&Submission::new("session-a", "submission-a"))
        .unwrap();
    store.mark_outcome_unknown("submission-a").unwrap();

    let reopened = AdmissionStore::new(&path).unwrap();
    let record = reopened.get("submission-a").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    assert_eq!(record.admission_id, None);
}

#[test]
fn receipt_records_distinct_admission_id() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    store
        .prepare(&Submission::new("session-a", "submission-a"))
        .unwrap();

    let record = store
        .record_receipt(
            "submission-a",
            &receipt("admission-a", "queued", Some(1)),
            false,
        )
        .unwrap();

    assert_eq!(record.submission_id, "submission-a");
    assert_eq!(record.admission_id.as_deref(), Some("admission-a"));
    assert_eq!(record.local_state, LocalState::Admitted);
    assert_eq!(record.gateway_status.as_deref(), Some("queued"));
    assert_eq!(record.execution_generation, Some(1));
}

#[test]
fn receipt_cannot_change_admission_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    store
        .prepare(&Submission::new("session-a", "submission-a"))
        .unwrap();
    store
        .record_receipt(
            "submission-a",
            &receipt("admission-a", "queued", None),
            false,
        )
        .unwrap();

    let err = store
        .record_receipt(
            "submission-a",
            &receipt("admission-b", "running", None),
            false,
        )
        .unwrap_err();
    assert!(err.to_string().contains("conflicts"));
}

#[test]
fn admission_id_is_unique_across_submissions() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    store
        .prepare(&Submission::new("session-a", "submission-a"))
        .unwrap();
    store
        .prepare(&Submission::new("session-a", "submission-b"))
        .unwrap();
    store
        .record_receipt(
            "submission-a",
            &receipt("admission-a", "queued", None),
            false,
        )
        .unwrap();

    let err = store
        .record_receipt(
            "submission-b",
            &receipt("admission-a", "queued", None),
            false,
        )
        .unwrap_err();
    assert!(err.to_string().contains("already assigned"));
}

#[test]
fn reconciliation_is_marked_separately() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    store
        .prepare(&Submission::new("session-a", "submission-a"))
        .unwrap();

    let record = store
        .record_receipt(
            "submission-a",
            &receipt("admission-a", "running", None),
            true,
        )
        .unwrap();

    assert_eq!(record.local_state, LocalState::ReceiptObserved);
}

#[test]
fn unknown_submission_and_bad_limit_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_store(tmp.path());
    let err = store.mark_outcome_unknown("missing").unwrap_err();
    assert!(err.to_string().contains("Unknown submission"));
    let err = store.list_records(0).unwrap_err();
    assert!(err.to_string().contains("limit"));
}

#[cfg(unix)]
#[test]
fn symlink_database_path_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target.sqlite3");
    std::fs::write(&target, "").unwrap();
    let link = tmp.path().join("link.sqlite3");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let err = AdmissionStore::new(&link).unwrap_err();
    assert!(err.to_string().contains("symbolic link"));
}
