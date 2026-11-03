#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Negative and positive contract tests for sandboxed read-only agent tools.

use hermes_harness::multiagent::sandbox_tools::{
    SandboxTool, ToolResult, record_sandbox_tool, run_sandbox_tool, run_sandbox_tool_with,
    sandbox_tool_argv, sandbox_tool_argv_with,
};
use hermes_harness::util::resolve_strict;

#[test]
fn no_shell_network_home_or_write_mount() {
    let dir = tempfile::tempdir().unwrap();
    let args = sandbox_tool_argv_with(dir.path(), "list-files", "", "bwrap", &|_| true).unwrap();
    assert!(args.iter().any(|a| a == "--unshare-all"));
    assert!(args.iter().any(|a| a == "--clearenv"));
    assert!(!args.iter().any(|a| a == "sh"));
    let root = resolve_strict(dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        args.windows(3)
            .any(|w| w[0] == "--ro-bind" && w[1] == root && w[2] == "/work")
    );
    assert!(!args.iter().any(|a| a == "--bind"));
}

#[test]
fn unknown_tool_and_invalid_pattern_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let error = sandbox_tool_argv(dir.path(), "bash", "", "bwrap").unwrap_err();
    assert!(error.to_string().contains("Unknown"));
    for pattern in ["\0", "\n"] {
        let error = sandbox_tool_argv(dir.path(), "search-fixed", pattern, "bwrap").unwrap_err();
        assert!(error.to_string().contains("pattern"));
    }
}

#[test]
fn authorization_precedes_bwrap_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let error = run_sandbox_tool(dir.path(), "list-files", "", false).unwrap_err();
    assert!(error.to_string().contains("authorization"));
    let error = run_sandbox_tool_with(dir.path(), "list-files", "", true, &|_| None).unwrap_err();
    assert!(error.to_string().contains("Bubblewrap unavailable"));
}

#[test]
fn receipt_write_once() {
    let dir = tempfile::tempdir().unwrap();
    let evidence = dir.path().join("tool.json");
    let result = ToolResult::new(SandboxTool::ListFiles, Some(0), false, "file.py", "", "abc");
    let digest = record_sandbox_tool(&result, &evidence).unwrap();
    assert_eq!(digest.len(), 64);
    assert_eq!(
        std::fs::read_to_string(&evidence).unwrap(),
        concat!(
            "{\"exit_code\": 0, \"host_execution\": false, \"output_sha256\": \"abc\", ",
            "\"sandbox\": \"bubblewrap-no-network-readonly-worktree\", \"stderr\": \"\", ",
            "\"stdout\": \"file.py\", \"timed_out\": false, \"tool\": \"list-files\"}\n"
        )
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&evidence).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let error = record_sandbox_tool(&result, &evidence).unwrap_err();
    assert!(error.to_string().contains("new path"));
}

#[cfg(unix)]
#[test]
fn sandbox_namespace_failure_refuses_result() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    let dir = tempfile::tempdir().unwrap();
    let bwrap = dir.path().join("bwrap");
    std::fs::write(
        &bwrap,
        "#!/bin/sh\nprintf '%s\\n' 'bwrap: setting up uid map: Permission denied' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = run_sandbox_tool_with(dir.path(), "list-files", "", true, &|_| Some(bwrap.clone()))
        .unwrap_err();
    let expected = if ["/usr/bin/prlimit", "/bin/prlimit"]
        .iter()
        .any(|path| Path::new(path).is_file())
    {
        "Sandbox unavailable or timed out; no host fallback"
    } else {
        "Resource limiter unavailable: refusing unconstrained execution"
    };
    assert!(error.to_string().contains(expected), "{error}");
}
