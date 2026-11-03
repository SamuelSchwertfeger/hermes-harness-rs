#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Check that the local communication probe cannot become provider execution.

use hermes_harness::gateway::gateway_socket_qualification::sandbox_command;

fn has(argv: &[String], needle: &str) -> bool {
    argv.iter().any(|part| part == needle)
}

#[test]
fn fixed_bwrap_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let argv = sandbox_command(tmp.path());
    assert!(has(&argv, "--unshare-net"));
    assert!(has(&argv, "--unshare-pid"));
    assert!(has(&argv, "--die-with-parent"));
    assert!(has(&argv, "--ro-bind"));
    assert!(has(&argv, "/gateway"));
    assert!(!has(&argv, "/home/hermes"));
    assert!(!has(&argv, "sudo"));
}

#[test]
fn command_has_no_provider_or_hermes_agent_invocation() {
    let tmp = tempfile::tempdir().unwrap();
    let argv = sandbox_command(tmp.path());
    assert_eq!(argv[argv.len() - 2], "-c");
    assert_eq!(argv[argv.len() - 3], "/usr/bin/python3");
    assert!(
        !argv
            .iter()
            .any(|part| part.to_lowercase().contains("api_key"))
    );
}
