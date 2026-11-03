//! Command-construction tests; no sudo, subprocess, Hermes or network execution.
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

use std::path::Path;

use hermes_harness::sandbox::live_sandbox_plan::{
    Role, SandboxInputs, SandboxPlanError, build_sandbox_plan,
};
use tempfile::TempDir;

const EXPECTED_READONLY_MOUNTS: usize = 3;

fn inputs(tmp: &Path) -> SandboxInputs {
    let work = tmp.join("work");
    let evidence = tmp.join("evidence");
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(&evidence).unwrap();
    SandboxInputs::new(
        Role::Worker,
        "hermes-job-test001",
        &work,
        &evidence,
        &Path::new("/usr/bin/env").canonicalize().unwrap(),
        &["hello"],
    )
}

fn s(path: &Path) -> String {
    path.canonicalize().unwrap().to_string_lossy().into_owned()
}

#[test]
fn worker_mount_is_writeable_but_evidence_read_only() {
    let tmp = TempDir::new().unwrap();
    let inputs = inputs(tmp.path());
    let args = build_sandbox_plan(&inputs).unwrap().systemd_argv;
    let index = args.iter().position(|a| a == "--bind").unwrap();
    assert_eq!(
        args[index..index + 3],
        [
            "--bind".to_owned(),
            s(&inputs.workspace),
            "/workspace".to_owned()
        ]
    );
    let index = args.iter().position(|a| *a == s(&inputs.evidence)).unwrap();
    assert_eq!(
        args[index - 1..index + 2],
        [
            "--ro-bind".to_owned(),
            s(&inputs.evidence),
            "/evidence".to_owned()
        ]
    );
    for flag in [
        "--unshare-net",
        "--unshare-pid",
        "--property=KillMode=control-group",
        "--uid=hermes",
    ] {
        assert!(args.iter().any(|a| a == flag));
    }
    assert!(!args.iter().any(|a| a == "/home/hermes/.hermes"));
    for (source, target) in [
        ("usr/lib", "/lib"),
        ("usr/lib64", "/lib64"),
        ("usr/bin", "/bin"),
        ("usr/sbin", "/sbin"),
    ] {
        assert!(
            args.windows(3)
                .any(|part| part == ["--symlink", source, target])
        );
    }
}

#[test]
fn reviewer_has_no_writable_mount() {
    let tmp = TempDir::new().unwrap();
    let mut inputs = inputs(tmp.path());
    inputs.role = Role::Reviewer;
    let args = build_sandbox_plan(&inputs).unwrap().systemd_argv;
    assert!(!args.iter().any(|a| a == "--bind"));
    assert_eq!(
        args.iter().filter(|a| *a == "--ro-bind").count(),
        EXPECTED_READONLY_MOUNTS
    );
}

#[test]
fn unsafe_unit_rejected() {
    for unit in ["evil", "hermes-job-../../tmp", "hermes-job-x;ls"] {
        let tmp = TempDir::new().unwrap();
        let mut inputs = inputs(tmp.path());
        inputs.unit = unit.to_owned();
        assert!(matches!(
            build_sandbox_plan(&inputs),
            Err(SandboxPlanError::Invalid(_))
        ));
    }
}

#[test]
fn symlink_mount_rejected() {
    let tmp = TempDir::new().unwrap();
    let mut inputs = inputs(tmp.path());
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&inputs.workspace, &link).unwrap();
    inputs.workspace = link;
    assert!(matches!(
        build_sandbox_plan(&inputs),
        Err(SandboxPlanError::Invalid(_))
    ));
}

#[test]
fn overlapping_mounts_rejected() {
    let tmp = TempDir::new().unwrap();
    let mut inputs = inputs(tmp.path());
    inputs.evidence = inputs.workspace.clone();
    assert!(matches!(
        build_sandbox_plan(&inputs),
        Err(SandboxPlanError::Invalid(_))
    ));
}

#[test]
fn resource_override_rejected() {
    let tmp = TempDir::new().unwrap();
    let mut inputs = inputs(tmp.path());
    inputs.memory_max = "unlimited".to_owned();
    assert!(matches!(
        build_sandbox_plan(&inputs),
        Err(SandboxPlanError::Invalid(_))
    ));
}

#[test]
fn executable_from_workspace_rejected() {
    let tmp = TempDir::new().unwrap();
    let mut inputs = inputs(tmp.path());
    let executable = inputs.workspace.join("agent");
    std::fs::write(&executable, "x").unwrap();
    inputs.executable = executable;
    assert!(matches!(
        build_sandbox_plan(&inputs),
        Err(SandboxPlanError::Invalid(_))
    ));
}

#[test]
fn stop_is_specific_unit_only() {
    let tmp = TempDir::new().unwrap();
    let plan = build_sandbox_plan(&inputs(tmp.path())).unwrap();
    assert_eq!(
        plan.stop_argv,
        ["/usr/bin/systemctl", "stop", "hermes-job-test001.service"]
    );
    assert!(plan.to_json().contains("systemd_argv"));
}
