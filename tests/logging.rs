#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! `HERMES_LOG` JSON logging on stderr.

use std::process::Command;

fn run(level: Option<&str>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hermes-skill-bridge"));
    command.arg("doctor").env_remove("HERMES_LOG");
    if let Some(level) = level {
        command.env("HERMES_LOG", level);
    }
    command.output().unwrap()
}

#[test]
fn info_logs_are_json_lines() {
    let out = run(Some("info"));
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8(out.stderr).unwrap();
    let mut seen = 0;
    for line in stderr.lines().filter(|line| line.starts_with('{')) {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        for key in ["ts", "level", "target", "msg", "bin"] {
            assert!(value.get(key).is_some(), "{key} missing in {line}");
        }
        assert_eq!(value["bin"], "hermes-skill-bridge");
        seen += 1;
    }
    assert!(seen >= 2, "{stderr}");
    assert!(stderr.contains("code=0"), "{stderr}");
    assert!(!stderr.contains("doctor"), "args must not be logged");
}

#[test]
fn unset_is_silent_and_invalid_warns_once() {
    assert!(run(None).stderr.is_empty());
    let stderr = String::from_utf8(run(Some("loud")).stderr).unwrap();
    assert_eq!(stderr.matches("HERMES_LOG must be").count(), 1, "{stderr}");
}
