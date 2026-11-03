#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Provider-free host probe contract tests.

use std::cell::RefCell;
use std::collections::BTreeMap;

use hermes_harness::operator::live_host_probe::{
    CommandOutput, HostProbeError, ProbeCommands, ProbeObservation, assess_observation, disabled,
    observe_unit, validate_pid_snapshot, write_probe_evidence,
};

const UNIT: &str = "hermes-verify-12345678";

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn rejects_arbitrary_units() {
    for unit in [
        "hermes-job-prod",
        "hermes-verify-1;rm",
        "other",
        "hermes-verify-../bad",
    ] {
        let err = ProbeCommands::new(unit).unwrap_err();
        assert!(err.to_string().contains("verification namespace"));
    }
}

#[test]
fn property_allowlist() {
    let commands = ProbeCommands::new(UNIT).unwrap();
    let err = commands.show("Environment").unwrap_err();
    assert!(err.to_string().contains("Unrecognized"));
    assert_eq!(
        commands.stop(),
        strings(&["/usr/bin/systemctl", "stop", &format!("{UNIT}.service")])
    );
}

#[test]
fn default_transport_is_disabled() {
    let err = observe_unit(&ProbeCommands::new(UNIT).unwrap(), &disabled).unwrap_err();
    assert!(err.to_string().contains("disabled"));
}

#[test]
fn readonly_exact_property_commands() {
    let calls: RefCell<Vec<Vec<String>>> = RefCell::new(Vec::new());
    let runner = |argv: &[String]| -> Result<CommandOutput, HostProbeError> {
        calls.borrow_mut().push(argv.to_vec());
        Ok(CommandOutput {
            returncode: 0,
            stdout: "test\n".into(),
            stderr: String::new(),
        })
    };
    let observations = observe_unit(&ProbeCommands::new(UNIT).unwrap(), &runner).unwrap();
    assert_eq!(observations["User"], "test");
    let calls = calls.borrow();
    assert!(
        calls
            .iter()
            .all(|call| call[..3] == strings(&["/usr/bin/systemctl", "show", "--property"])[..])
    );
    assert_eq!(calls.len(), observations.len());
}

#[test]
fn nonzero_runner_fails_closed() {
    let failure = |_: &[String]| -> Result<CommandOutput, HostProbeError> {
        Ok(CommandOutput {
            returncode: 1,
            stdout: String::new(),
            stderr: "denied".into(),
        })
    };
    let err = observe_unit(&ProbeCommands::new(UNIT).unwrap(), &failure).unwrap_err();
    assert!(err.to_string().contains("failed"));
}

#[test]
fn exact_effective_controls() {
    let mut values: BTreeMap<String, String> = [
        ("ActiveState", "active"),
        ("MainPID", "42"),
        ("User", "hermes"),
        ("MemoryMax", "536870912"),
        ("CPUQuotaPerSecUSec", "500ms"),
        ("TasksMax", "32"),
        ("KillMode", "control-group"),
        (
            "ControlGroup",
            "/system.slice/hermes-verify-12345678.service",
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    assert!(assess_observation(&values).is_empty());
    values.insert("MemoryMax".into(), "infinity".into());
    assert!(assess_observation(&values).contains(&"MemoryMax".to_owned()));
    values.insert("MainPID".into(), "0".into());
    assert!(assess_observation(&values).contains(&"MainPID".to_owned()));
}

#[test]
fn process_snapshot_refuses_survivors() {
    validate_pid_snapshot(&[100, 101], &[]).unwrap();
    let err = validate_pid_snapshot(&[100, 101], &[101]).unwrap_err();
    assert!(err.to_string().contains("remained"));
    let err = validate_pid_snapshot(&[], &[]).unwrap_err();
    assert!(err.to_string().contains("Invalid or empty"));
}

#[test]
fn evidence_is_unapproved_and_exclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("evidence.json");
    let values = BTreeMap::from([("User".to_owned(), "hermes".to_owned())]);
    let observation = ProbeObservation::new(UNIT, values, vec![100], vec![], "inactive");
    write_probe_evidence(&path, &observation).unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(payload["qualified"], serde_json::Value::Bool(false));
    let err = write_probe_evidence(&path, &observation).unwrap_err();
    assert!(err.to_string().contains("already exists"));
}
