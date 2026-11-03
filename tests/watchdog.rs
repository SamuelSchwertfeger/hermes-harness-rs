#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Offline watchdog lifecycle regression tests.

#[cfg(unix)]
use std::sync::atomic::AtomicBool;

#[cfg(unix)]
use hermes_harness::core::watchdog::Outcome;
use hermes_harness::core::watchdog::{Budget, SupervisionOptions, supervise};

#[cfg(unix)]
fn argv(script: &str) -> Vec<String> {
    vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

#[cfg(unix)]
#[test]
fn completion() {
    let dir = tempfile::tempdir().unwrap();
    let options = SupervisionOptions::new(dir.path(), dir.path());
    let result = supervise(&argv("echo 42"), &options, None).unwrap();
    assert_eq!(result.outcome, Outcome::Completed);
    assert_eq!(result.returncode, Some(0));
    assert!(!result.group_alive);
    assert_eq!(
        std::fs::read_to_string(&result.stdout_path).unwrap().trim(),
        "42"
    );
    let text = std::fs::read_to_string(dir.path().join(&result.run_id).join("run.json")).unwrap();
    let record: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(record["run_id"], result.run_id.as_str());
}

#[cfg(unix)]
#[test]
fn deadline() {
    let dir = tempfile::tempdir().unwrap();
    let mut options = SupervisionOptions::new(dir.path(), dir.path());
    options.budget = Budget {
        deadline_seconds: 0.1,
        grace_seconds: 0.3,
        poll_seconds: 0.01,
    };
    let result = supervise(&argv("sleep 10"), &options, None).unwrap();
    assert_eq!(result.outcome, Outcome::TimedOut);
    assert!(!result.group_alive);
}

#[cfg(unix)]
#[test]
fn cancel() {
    let dir = tempfile::tempdir().unwrap();
    let options = SupervisionOptions::new(dir.path(), dir.path());
    let cancel = AtomicBool::new(true);
    let result = supervise(&argv("sleep 10"), &options, Some(&cancel)).unwrap();
    assert_eq!(result.outcome, Outcome::Cancelled);
    assert!(!result.group_alive);
}

#[test]
fn bad_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut options = SupervisionOptions::new(dir.path(), dir.path());
    options.budget = Budget {
        deadline_seconds: 0.0,
        ..Budget::default()
    };
    assert!(supervise(&["echo".to_owned(), "bad".to_owned()], &options, None).is_err());
}
