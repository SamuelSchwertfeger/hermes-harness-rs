#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Sandbox profile is inspectable but never executed by these tests.

use hermes_harness::multiagent::sandbox_profile::{SandboxSpec, bubblewrap_argv};

#[test]
fn bubblewrap_profile_denies_network_and_root_writes() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("pytest");
    std::fs::write(&executable, "# placeholder").unwrap();
    let command = bubblewrap_argv(&SandboxSpec::new(dir.path(), &executable, &["-q"])).unwrap();
    assert!(command.iter().any(|item| item == "--unshare-all"));
    assert!(
        command
            .iter()
            .position(|item| item == "--clearenv")
            .unwrap()
            < command.iter().position(|item| item == "--setenv").unwrap()
    );
    for (name, value) in [
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/tmp"),
        ("TMPDIR", "/tmp"),
    ] {
        assert!(
            command
                .windows(3)
                .any(|args| args == ["--setenv", name, value])
        );
    }
    let bind = command.iter().position(|item| item == "--ro-bind").unwrap();
    assert_eq!(command[bind + 1], "/");
    assert!(command.iter().any(|item| item == "--tmpfs"));
    assert!(command.iter().any(|item| item == "--"));
    assert_eq!(
        command[command.len() - 2..],
        [executable.to_string_lossy().into_owned(), "-q".to_owned()]
    );
}

#[test]
fn sandbox_refuses_bad_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("python");
    std::fs::write(&executable, "").unwrap();
    let spec = SandboxSpec::new(dir.path(), &executable, &["-m", "pytest"]).with_max_seconds(0);
    let error = bubblewrap_argv(&spec).unwrap_err();
    assert!(error.to_string().contains("Invalid bubblewrap"));
}

#[test]
fn sandbox_refuses_command_injection() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("python");
    std::fs::write(&executable, "").unwrap();
    let error = bubblewrap_argv(&SandboxSpec::new(dir.path(), &executable, &["a\0b"])).unwrap_err();
    assert!(error.to_string().contains("argument"));
}
