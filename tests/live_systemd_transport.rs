//! No privileged subprocess calls: deterministic fake systemctl responses.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
#![cfg(unix)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use hermes_harness::sandbox::live_sandbox_plan::{
    Role, SandboxInputs, SandboxPlan, build_sandbox_plan,
};
use hermes_harness::services::live_systemd_transport::{
    CompletedProcess, RestrictedSystemdTransport, SystemdTransportError,
};
use tempfile::TempDir;

fn plan(tmp: &Path) -> SandboxPlan {
    let workspace = tmp.join("workspace");
    let evidence = tmp.join("evidence");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&evidence).unwrap();
    build_sandbox_plan(&SandboxInputs::new(
        Role::Worker,
        "hermes-job-qualification",
        &workspace,
        &evidence,
        &Path::new("/usr/bin/env").canonicalize().unwrap(),
        &["--version"],
    ))
    .unwrap()
}

fn err_text<T>(result: Result<T, SystemdTransportError>) -> String {
    match result {
        Ok(_) => panic!("expected error"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn denied_without_explicit_runner() {
    let tmp = TempDir::new().unwrap();
    let plan = plan(tmp.path());
    let transport = RestrictedSystemdTransport::new(plan.clone()).unwrap();
    assert_eq!(transport.plan(), &plan);
    assert!(err_text(transport.invoke(&plan.systemd_argv)).contains("Host commands disabled"));
}

#[test]
fn exact_command_allowlist() {
    let tmp = TempDir::new().unwrap();
    let plan = plan(tmp.path());
    let calls: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
    let recorder = Rc::clone(&calls);
    let transport = RestrictedSystemdTransport::with_runner(
        plan.clone(),
        Box::new(move |args| {
            recorder.borrow_mut().push(args.to_vec());
            Ok(CompletedProcess {
                returncode: 0,
                stdout: String::new(),
            })
        }),
    )
    .unwrap();
    transport.invoke(&plan.systemd_argv).unwrap();
    transport.invoke(&plan.stop_argv).unwrap();
    let expected = vec![plan.systemd_argv.clone(), plan.stop_argv.clone()];
    assert_eq!(*calls.borrow(), expected);
    let other = ["/usr/bin/systemctl", "stop", "ssh.service"].map(String::from);
    assert!(err_text(transport.invoke(&other)).contains("not part of"));
    assert_eq!(*calls.borrow(), expected);
}

#[test]
fn inspection_requires_unit_match() {
    let tmp = TempDir::new().unwrap();
    let transport = RestrictedSystemdTransport::new(plan(tmp.path())).unwrap();
    assert!(err_text(transport.is_active("other")).contains("Cross-unit"));
}

#[test]
fn active_requires_pid() {
    let cases = [
        ("active", "123", true),
        ("active", "0", false),
        ("inactive", "123", false),
    ];
    for (active, mainpid, expected) in cases {
        let tmp = TempDir::new().unwrap();
        let plan = plan(tmp.path());
        let transport = RestrictedSystemdTransport::with_runner(
            plan.clone(),
            Box::new(move |args| {
                let stdout = if args.iter().any(|a| a == "ActiveState") {
                    active
                } else {
                    mainpid
                };
                Ok(CompletedProcess {
                    returncode: 0,
                    stdout: stdout.to_owned(),
                })
            }),
        )
        .unwrap();
        assert_eq!(transport.is_active(&plan.unit).unwrap(), expected);
    }
}

#[test]
fn nonzero_status_is_uncertain() {
    let tmp = TempDir::new().unwrap();
    let plan = plan(tmp.path());
    let transport = RestrictedSystemdTransport::with_runner(
        plan.clone(),
        Box::new(|_args| {
            Ok(CompletedProcess {
                returncode: 1,
                stdout: String::new(),
            })
        }),
    )
    .unwrap();
    assert!(err_text(transport.invoke(&plan.systemd_argv)).contains("outcome must be reconciled"));
}

#[test]
fn default_transport_cannot_run_host_commands() {
    let tmp = TempDir::new().unwrap();
    let plan = plan(tmp.path());
    let transport = RestrictedSystemdTransport::new(plan.clone()).unwrap();
    assert!(err_text(transport.is_inactive(&plan.unit)).contains("disabled"));
}
