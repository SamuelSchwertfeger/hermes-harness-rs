#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline multiagent CLI output, byte-compared with Python `json.dumps(indent=2)`.

use clap::Parser;
use hermes_harness::multiagent::cli::{Args, run};

fn output(argv: &[&str]) -> String {
    let args =
        Args::try_parse_from(std::iter::once("multiagent").chain(argv.iter().copied())).unwrap();
    run(&args).unwrap()
}

#[test]
fn doctor_matches_python() {
    let expected = r#"{
  "offline": true,
  "live_gateway": "unqualified",
  "provider_execution": "disabled",
  "concurrency": 1,
  "command_isolation": "not provided by offline doubles"
}"#;
    assert_eq!(output(&["doctor"]), expected);
}

#[test]
fn plan_matches_python() {
    let example = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/multiagent-example.json");
    let expected = r#"{
  "ordered_nodes": [
    "first",
    "second"
  ],
  "mode": "offline",
  "parallel": false
}"#;
    assert_eq!(output(&["plan", example]), expected);
}

#[test]
fn demo_matches_python() {
    let expected = r#"{
  "mode": "offline",
  "state": "pending_human_decision",
  "tasks": [
    {
      "node": "first",
      "state": "completed"
    },
    {
      "node": "second",
      "state": "completed"
    }
  ],
  "decision": {
    "approved": false,
    "integrated": false,
    "run_id": "demo",
    "state": "pending"
  },
  "note": "Temporary demonstration removed; no live provider or merge executed"
}"#;
    assert_eq!(output(&["demo"]), expected);
}

#[test]
fn unknown_run_is_refused() {
    let state = tempfile::tempdir().unwrap();
    let args = Args::try_parse_from([
        "multiagent",
        "--state",
        state.path().to_str().unwrap(),
        "inspect",
        "missing",
    ])
    .unwrap();
    assert!(run(&args).is_err());
}
