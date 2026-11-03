#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Provider-free containment tests for supervised sandbox plans.

use hermes_harness::operator::live_operator_safety::{
    extract_mounts, validate_role_sandbox, validate_separate_roles,
};

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn base() -> Vec<String> {
    strings(&[
        "/usr/bin/systemd-run",
        "--unit=hermes-job-test",
        "/usr/bin/bwrap",
        "--unshare-net",
        "--unshare-pid",
        "--unshare-user",
        "--die-with-parent",
        "--ro-bind",
        "/usr",
        "/usr",
    ])
}

fn tail(items: &[&str]) -> Vec<String> {
    let mut out = base();
    out.extend(strings(items));
    out
}

fn worker() -> Vec<String> {
    tail(&[
        "--bind",
        "/safe/worker",
        "/workspace",
        "--ro-bind",
        "/safe/evidence",
        "/evidence",
        "--chdir",
        "/workspace",
        "/usr/bin/python3",
    ])
}

fn reviewer() -> Vec<String> {
    tail(&[
        "--ro-bind",
        "/safe/review",
        "/workspace",
        "--ro-bind",
        "/safe/evidence",
        "/evidence",
        "--chdir",
        "/workspace",
        "/usr/bin/python3",
    ])
}

fn message<T: std::fmt::Debug>(
    result: Result<T, hermes_harness::operator::live_operator_safety::OperatorSafetyError>,
) -> String {
    result.unwrap_err().to_string()
}

#[test]
fn separate_sandbox_mounts_accepted() {
    validate_separate_roles(&worker(), &reviewer()).unwrap();
    assert_eq!(
        extract_mounts(&worker()).unwrap().writable,
        [("/safe/worker".to_owned(), "/workspace".to_owned())]
    );
}

#[test]
fn reviewer_writable_mount_denied() {
    let mut argv = reviewer();
    let at = argv.len() - 3;
    for (offset, item) in ["--bind", "/safe/extra", "/extra"].iter().enumerate() {
        argv.insert(at + offset, (*item).to_owned());
    }
    assert!(message(validate_role_sandbox(&argv, "reviewer")).contains("read-only"));
}

#[test]
fn flag_in_command_arguments_does_not_qualify_mount() {
    let broken = tail(&[
        "--ro-bind",
        "/safe/evidence",
        "/evidence",
        "--chdir",
        "/workspace",
        "/usr/bin/python3",
        "--bind",
    ]);
    assert!(message(validate_role_sandbox(&broken, "worker")).contains("writable"));
}

#[test]
fn reviewer_worker_workspace_alias_denied() {
    let unsafe_argv: Vec<String> = reviewer()
        .into_iter()
        .map(|v| {
            if v == "/safe/review" {
                "/safe/worker".to_owned()
            } else {
                v
            }
        })
        .collect();
    assert!(
        message(validate_separate_roles(&worker(), &unsafe_argv))
            .contains("worker writable workspace")
    );
}

#[test]
fn missing_evidence_denied() {
    let broken = tail(&[
        "--bind",
        "/safe/worker",
        "/workspace",
        "--chdir",
        "/workspace",
    ]);
    assert!(message(validate_role_sandbox(&broken, "worker")).contains("Evidence"));
}

#[test]
fn duplicate_mount_source_denied() {
    let broken: Vec<String> = worker()
        .into_iter()
        .map(|v| {
            if v == "/safe/evidence" {
                "/safe/worker".to_owned()
            } else {
                v
            }
        })
        .collect();
    assert!(message(validate_role_sandbox(&broken, "worker")).contains("reused"));
}
