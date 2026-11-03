#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Runs every `hermes-*` binary.

use std::process::{Command, Output};

fn run(exe: &str, args: &[&str]) -> Output {
    Command::new(exe).args(args).output().unwrap()
}

#[test]
fn native_game_plan_success() {
    let dir = tempfile::tempdir().unwrap();
    let plan = dir.path().join("plan.json");
    std::fs::write(
        &plan,
        r#"{"id":"task","objective":"Tests","nodes":[{"id":"one","objective":"Write tests","owned_paths":["tests/a.py"]}]}"#,
    )
    .unwrap();
    let out = run(
        env!("CARGO_BIN_EXE_hermes-native"),
        &["game-plan", "--plan", plan.to_str().unwrap()],
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(stdout.starts_with("{\n  "), "{stdout}");
}

#[test]
fn native_missing_plan_exits_two() {
    let out = run(
        env!("CARGO_BIN_EXE_hermes-native"),
        &["game-plan", "--plan", "/nonexistent/plan.json"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.starts_with(r#"{"error": ""#), "{stderr}");
}

#[test]
fn bridge_doctor_success() {
    let out = run(env!("CARGO_BIN_EXE_hermes-skill-bridge"), &["doctor"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.starts_with(r#"{"#) && stdout.contains(r#""ok": true"#),
        "{stdout}"
    );
}

#[test]
fn bridge_error_prints_on_stdout_and_exits_two() {
    let out = run(
        env!("CARGO_BIN_EXE_hermes-skill-bridge"),
        &["plan", "--file", "/nonexistent/plan.json", "--session", "s"],
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(2), "{stdout}");
    assert!(stdout.contains(r#""ok": false"#), "{stdout}");
}
fn stdout_of(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// Bytes printed by CPython 3.14 for the same fixture (`srcs`/`tgt` relative paths).
const SKILL_INSTALL_DRY_RUN: &str = "\
srcs/lets-investigate/SKILL.md -> tgt/hermes-harness/lets-investigate/SKILL.md
srcs/game-plan/SKILL.md -> tgt/hermes-harness/game-plan/SKILL.md
srcs/agents-assemble/SKILL.md -> tgt/hermes-harness/agents-assemble/SKILL.md
srcs/what-we-know/SKILL.md -> tgt/hermes-harness/what-we-know/SKILL.md
srcs/cyber-warrior/SKILL.md -> tgt/hermes-harness/cyber-warrior/SKILL.md
DRY RUN: no skill files changed. Supply --apply after reviewing.
";

fn skill_fixture(dir: &std::path::Path) {
    for name in [
        "lets-investigate",
        "game-plan",
        "agents-assemble",
        "what-we-know",
        "cyber-warrior",
    ] {
        let skill = dir.join("srcs").join(name);
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), format!("# {name}\n")).unwrap();
    }
    std::fs::create_dir(dir.join("tgt")).unwrap();
}

#[test]
fn skill_install_dry_run_matches_python() {
    let dir = tempfile::tempdir().unwrap();
    skill_fixture(dir.path());
    let out = Command::new(env!("CARGO_BIN_EXE_hermes-skill-install"))
        .current_dir(dir.path())
        .args(["--source", "srcs", "--target", "tgt"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out).replace('\\', "/"), SKILL_INSTALL_DRY_RUN);
    assert!(!dir.path().join("tgt/hermes-harness").exists());
}

#[test]
fn skill_install_apply_accepts_abbreviation() {
    let dir = tempfile::tempdir().unwrap();
    skill_fixture(dir.path());
    // `--appl` is an argparse-style abbreviation of `--apply`.
    let out = Command::new(env!("CARGO_BIN_EXE_hermes-skill-install"))
        .current_dir(dir.path())
        .args(["--source", "srcs", "--target", "tgt", "--appl"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    assert!(stdout_of(&out).ends_with("start a fresh Hermes session.\n"));
    assert!(
        dir.path()
            .join("tgt/hermes-harness/game-plan/SKILL.md")
            .is_file()
    );
}

#[test]
fn skill_install_errors() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope");
    let out = run(
        env!("CARGO_BIN_EXE_hermes-skill-install"),
        &[
            "--source",
            missing.to_str().unwrap(),
            "--target",
            dir.path().to_str().unwrap(),
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(stderr_of(&out).contains("Skill source root must be a real directory"));
    let out = run(env!("CARGO_BIN_EXE_hermes-skill-install"), &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_of(&out).contains("--source"));
}

#[test]
fn supervised_demo_then_status() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("demo");
    let root_arg = root.to_str().unwrap();
    let out = run(
        env!("CARGO_BIN_EXE_hermes-supervised-demo"),
        &["--root", root_arg],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let text = stdout_of(&out);
    assert!(text.ends_with("}\n"), "{text}");
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    // Key order and the deterministic fields match CPython 3.14.
    assert!(text.starts_with(
        "{\n  \"mode\": \"OFFLINE SCRIPTED (NO HERMES)\",\n  \"worker_admission\": \"scripted-worker-scripted\",\n  \"reviewer_admission\": \"scripted-reviewer-scripted\",\n  \"audit_sha256\": \""
    ), "{text}");
    assert_eq!(report["approved"], false);
    assert_eq!(report["audit_sha256"].as_str().unwrap().len(), 64);

    // Reusing the root is a usage error (argparse `parser.error`).
    let again = run(
        env!("CARGO_BIN_EXE_hermes-supervised-demo"),
        &["--root", root_arg],
    );
    assert_eq!(again.status.code(), Some(2));
    assert!(stderr_of(&again).contains("Root must not exist; old evidence is never overwritten"));

    let status = run(
        env!("CARGO_BIN_EXE_hermes-supervised-status"),
        &["--root", root_arg],
    );
    assert_eq!(status.status.code(), Some(0), "{}", stderr_of(&status));
    let text = stdout_of(&status);
    assert!(
        text.starts_with(
            "{\n  \"task_id\": \"scripted-demo\",\n  \"evidence\": {\n    \"reservation.json\": true,\n"
        ),
        "{text}"
    );
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["approved"], false);
    let admissions = parsed["admissions"].as_array().unwrap();
    assert_eq!(admissions.len(), 2);
    // Rows are ordered by random submission id, so compare as a set.
    let mut ids: Vec<&str> = admissions
        .iter()
        .map(|row| row["admission_id"].as_str().unwrap())
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["scripted-reviewer-scripted", "scripted-worker-scripted"]
    );
    assert!(
        admissions
            .iter()
            .all(|row| row["state"] == "receipt_observed")
    );
}

#[test]
fn supervised_usage_errors_exit_two() {
    for exe in [
        env!("CARGO_BIN_EXE_hermes-supervised-demo"),
        env!("CARGO_BIN_EXE_hermes-supervised-status"),
    ] {
        let out = run(exe, &[]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        assert!(stderr_of(&out).contains("--root"));
    }
}

#[test]
fn gateway_probes_help_and_errors() {
    for (exe, usage, failure) in [
        (
            env!("CARGO_BIN_EXE_hermes-gateway-contract-probe"),
            "usage: gateway_contract_probe [-h] checkout",
            "Inspection failed: ",
        ),
        (
            env!("CARGO_BIN_EXE_hermes-gateway-lookup-probe"),
            "usage: gateway_lookup_probe [-h] source_root",
            "Lookup inspection failed: ",
        ),
    ] {
        let help = run(exe, &["-h"]);
        assert_eq!(help.status.code(), Some(0));
        assert!(stdout_of(&help).starts_with(usage));
        let missing = run(exe, &["/nonexistent"]);
        assert_eq!(missing.status.code(), Some(2));
        assert!(missing.stdout.is_empty());
        assert!(stderr_of(&missing).starts_with(failure));
        let none = run(exe, &[]);
        assert_eq!(none.status.code(), Some(2));
        assert!(stderr_of(&none).starts_with(usage));
    }
}

#[test]
fn socket_qualification_help_and_usage() {
    let exe = env!("CARGO_BIN_EXE_hermes-gateway-socket-qualification");
    let help = run(exe, &["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout_of(&help).contains("--qualify"));
    let none = run(exe, &[]);
    assert_eq!(none.status.code(), Some(2));
    assert!(stderr_of(&none).contains("--qualify"));
}

#[test]
fn live_host_probe_cli() {
    let exe = env!("CARGO_BIN_EXE_hermes-live-host-probe");
    let help = run(exe, &["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout_of(&help).starts_with("usage:"));
    let none = run(exe, &[]);
    assert_eq!(none.status.code(), Some(2));
    assert!(stderr_of(&none).contains("--unit <UNIT>"));
    let bad = run(exe, &["--unit", "x"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(stderr_of(&bad).contains(
        "HostProbeError: Unit identity is outside the disposable verification namespace"
    ));
}

#[test]
fn live_operator_cli() {
    let exe = env!("CARGO_BIN_EXE_hermes-live-operator");
    let help = run(exe, &["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(stdout_of(&help).starts_with("usage:"));
    let none = run(exe, &[]);
    assert_eq!(none.status.code(), Some(2));
    assert!(stderr_of(&none).contains("live_operator_cli: error:"));
}

#[test]
fn gateway_probes_accept_separator_and_help_after_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("gateway")).unwrap();
    std::fs::write(
        dir.path().join("gateway/session_controls.py"),
        "# offline\n",
    )
    .unwrap();
    for exe in [
        env!("CARGO_BIN_EXE_hermes-gateway-contract-probe"),
        env!("CARGO_BIN_EXE_hermes-gateway-lookup-probe"),
    ] {
        let plain = run(exe, &[dir.path().to_str().unwrap()]);
        let separated = run(exe, &["--", dir.path().to_str().unwrap()]);
        assert_eq!(separated.status.code(), plain.status.code());
        assert_eq!(separated.stdout, plain.stdout);
        assert_eq!(separated.stderr, plain.stderr);
        let help = run(exe, &["/nonexistent", "--help"]);
        assert_eq!(help.status.code(), Some(0));
        assert_eq!(help.stdout, run(exe, &["--help"]).stdout);
        assert!(help.stderr.is_empty());
        assert_eq!(run(exe, &["--oops"]).status.code(), Some(2));
    }
}

#[test]
fn operator_parsers_accept_prefixes_and_reject_missing_values() {
    let host = env!("CARGO_BIN_EXE_hermes-live-host-probe");
    let prefixed = run(host, &["--un", "x", "--exec"]);
    let full = run(host, &["--unit", "x", "--execute-readonly"]);
    assert_eq!(prefixed.status.code(), Some(1));
    assert_eq!(prefixed.stderr, full.stderr);
    for args in [
        vec!["--unit", "--execute-readonly"],
        vec!["--unit", "--oops"],
    ] {
        let out = run(host, &args);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
    let operator = env!("CARGO_BIN_EXE_hermes-live-operator");
    let full = [
        "--bindings",
        "/no/b",
        "--services",
        "/no/s",
        "--admissions",
        "/no/a",
        "--task-id",
        "t",
    ];
    let abbreviated = [
        "--bind", "/no/b", "--serv", "/no/s", "--adm", "/no/a", "--task", "t",
    ];
    let out = run(operator, &abbreviated);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stderr, run(operator, &full).stderr);
    for flag in ["--bindings", "--services", "--admissions", "--task-id"] {
        let out = run(operator, &[flag, "--oops"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
    }
}

#[cfg(any(unix, windows))]
#[test]
fn gateway_probe_preserves_non_utf8_path() {
    #[cfg(unix)]
    let name = {
        use std::os::unix::ffi::OsStringExt as _;
        std::ffi::OsString::from_vec(vec![b'p', 0xff])
    };
    #[cfg(windows)]
    let name = {
        use std::os::windows::ffi::OsStringExt as _;
        std::ffi::OsString::from_wide(&[0x70, 0xd800])
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(name);
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("gateway")).unwrap();
    std::fs::write(root.join("gateway/session_controls.py"), "# offline\n").unwrap();
    let twin = dir.path().join("p");
    std::fs::create_dir_all(twin.join("gateway")).unwrap();
    std::fs::write(twin.join("gateway/session_controls.py"), "# offline\n").unwrap();
    for exe in [
        env!("CARGO_BIN_EXE_hermes-gateway-contract-probe"),
        env!("CARGO_BIN_EXE_hermes-gateway-lookup-probe"),
    ] {
        // A lossy argv would turn `root` into a path that does not exist.
        let out = Command::new(exe).arg("--").arg(&root).output().unwrap();
        let reference = Command::new(exe).arg("--").arg(&twin).output().unwrap();
        assert_eq!(out.status.code(), reference.status.code());
        assert_eq!(out.stderr, reference.stderr);
    }
}
