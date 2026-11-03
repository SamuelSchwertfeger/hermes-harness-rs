#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Top-level and native diagnostic CLI output, byte-compared with the Python console script.

use clap::Parser;
use hermes_harness::cli::{CliError, run_argv};
use hermes_harness::native::main_cli::{Args, run};
use hermes_harness::native::skill_bridge::{Args as BridgeArgs, handle, render, render_error};

fn output(argv: &[&str]) -> String {
    run_argv(std::iter::once("hermes-harness").chain(argv.iter().copied())).unwrap()
}

#[test]
fn route_matches_python() {
    let planner = r#"{
  "role": "planner",
  "backend": "codex",
  "model": "gpt-6.1-sol",
  "tool_access": false,
  "read_only": true,
  "max_seconds": 120,
  "max_output_tokens": 2048,
  "gpu_slots": 0,
  "enabled": true
}"#;
    let scout = r#"{
  "role": "scout",
  "backend": "ollama",
  "model": "granite4:3b",
  "tool_access": false,
  "read_only": true,
  "max_seconds": 60,
  "max_output_tokens": 512,
  "gpu_slots": 1,
  "enabled": true
}"#;
    let reviewer = r#"{
  "role": "reviewer",
  "backend": "ollama",
  "model": "granite4:3b",
  "tool_access": false,
  "read_only": true,
  "max_seconds": 90,
  "max_output_tokens": 1024,
  "gpu_slots": 1,
  "enabled": true
}"#;
    assert_eq!(output(&["route", "planner"]), planner);
    assert_eq!(output(&["route", "scout"]), scout);
    assert_eq!(output(&["route", "reviewer"]), reviewer);
}

#[test]
fn disabled_route_fails() {
    let error = run_argv(["hermes-harness", "route", "cyber"]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Route cyber is disabled pending evaluation"
    );
}

#[test]
fn ticket_flow_matches_python() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("s");
    let state = state.to_str().unwrap();
    let report = |stage: &str, evidence: &str| {
        format!("{{\n  \"id\": \"t1\",\n  \"stage\": \"{stage}\",\n  \"evidence\": {evidence}\n}}")
    };
    assert_eq!(
        output(&["--state", state, "new", "t1", "hello"]),
        report("investigate", "{}")
    );
    let facts = "{\n    \"facts\": \"F\"\n  }";
    assert_eq!(
        output(&["--state", state, "evidence", "t1", "facts", "F"]),
        report("investigate", facts)
    );
    assert_eq!(
        output(&["--state", state, "advance", "t1", "consolidate-facts"]),
        report("consolidate-facts", facts)
    );
    assert_eq!(
        output(&["--state", state, "approve", "t1", "plan"]),
        report("consolidate-facts", facts)
    );
    assert_eq!(
        output(&["--state", state, "show", "t1"]),
        report("consolidate-facts", facts)
    );
    let baton = output(&["--state", state, "baton", "t1"]);
    let expected = std::path::Path::new(state)
        .join("tickets")
        .join("t1")
        .join("handoffs")
        .join("BATON.md");
    assert_eq!(baton, expected.display().to_string());
    assert_eq!(
        std::fs::read_to_string(expected).unwrap(),
        "# Baton: t1\n\nStage: consolidate-facts\n\n\
         Read ../state.json, ../facts.md, ../plan.md and briefs before proceeding.\n"
    );
}

#[test]
fn multiagent_is_forwarded() {
    let text = output(&["multiagent", "doctor"]);
    assert!(text.starts_with("{\n  \"offline\": true,"), "{text}");
}

#[test]
fn bad_arguments_are_usage_errors() {
    assert!(matches!(
        run_argv(["hermes-harness", "route", "nobody"]),
        Err(CliError::Usage(_))
    ));
    assert!(matches!(
        run_argv(["hermes-harness"]),
        Err(CliError::Usage(_))
    ));
}

#[test]
fn native_diagnostics_fail_like_python() {
    let args =
        Args::try_parse_from(["native", "game-plan", "--plan", "/nonexistent/plan.json"]).unwrap();
    assert_eq!(
        run(&args).unwrap_err().report().unwrap(),
        r#"{"error": "WorkflowError", "message": "Plan must be a real JSON file of at most 64KiB"}"#
    );
    let args = Args::try_parse_from([
        "native",
        "agents-assemble",
        "--repo",
        ".",
        "--plan",
        "x",
        "--state",
        "y",
    ])
    .unwrap();
    assert_eq!(
        run(&args).unwrap_err().report().unwrap(),
        r#"{"error": "WorkflowError", "message": "Explicit --confirm-offline is required"}"#
    );
}

#[test]
fn bridge_output_matches_python() {
    let doctor = handle(&BridgeArgs::try_parse_from(["bridge", "doctor"]).unwrap()).unwrap();
    assert_eq!(
        render(&doctor).unwrap(),
        r#"{"durable_inspection": true, "human_grant_required": true, "live_execution_qualified": false, "local_model_adapters": "opt-in-unqualified", "native_skills": "instruction-only", "ok": true, "plan_validation": true, "tool_sandbox_qualified": false}"#
    );
    let args = BridgeArgs::try_parse_from([
        "bridge",
        "inspect",
        "--state",
        "/nonexistent",
        "--run-id",
        "x",
        "--session",
        "s",
    ])
    .unwrap();
    let error = handle(&args).unwrap_err();
    assert_eq!(
        render_error(&error).unwrap(),
        r#"{"error": "State root must be an existing real directory", "ok": false}"#
    );
}

#[test]
fn argparse_repeats_and_abbreviations() {
    // top level: repeated --state (last wins) and unique prefix --sta
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    let text = output(&[
        "--state",
        a.to_str().unwrap(),
        "--sta",
        b.to_str().unwrap(),
        "new",
        "t1",
        "s",
    ]);
    assert!(text.contains("\"id\": \"t1\""), "{text}");
    assert!(b.join("tickets").exists() && !a.exists());
    // native diagnostics: sub-command long options
    let args = Args::try_parse_from([
        "native", "inspect", "--st", "x", "--state", "y", "--run", "r1",
    ])
    .unwrap();
    assert!(format!("{args:?}").contains("\"y\""));
    // bridge
    let args = BridgeArgs::try_parse_from([
        "bridge", "inspect", "--st", "x", "--state", "y", "--run-i", "r", "--sess", "s",
    ])
    .unwrap();
    assert!(format!("{args:?}").contains("\"y\""));
}

#[test]
fn symlinked_state_with_trailing_slash_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    if !make_dir_symlink(&real, &link) {
        return;
    }
    let slashed = format!("{}/", link.to_str().unwrap());
    let args = BridgeArgs::try_parse_from([
        "bridge",
        "inspect",
        "--state",
        &slashed,
        "--run-id",
        "x",
        "--session",
        "s",
    ])
    .unwrap();
    let error = handle(&args).unwrap_err();
    assert_eq!(
        render_error(&error).unwrap(),
        r#"{"error": "State root must be an existing real directory", "ok": false}"#
    );
}

#[cfg(unix)]
fn make_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(windows)]
fn make_dir_symlink(target: &std::path::Path, link: &std::path::Path) -> bool {
    std::os::windows::fs::symlink_dir(target, link).is_ok()
}
