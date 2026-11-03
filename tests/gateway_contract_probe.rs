#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Offline tests of the Hermes RPC source inspector (no inference or imports).

use hermes_harness::gateway::gateway_contract_probe::{inspect_checkout, inspect_source};

const SOURCE: &str = "
class Gateway:
    async def submit(self, params):
        return {'admission_id': 'abc', 'state': 'queued'}
    def receipt(self, params):
        return {'submission_id': 'def', 'state': 'running'}
    def cancel(self, params):
        return {'status': 'requested'}
";

fn message<T>(result: Result<T, impl std::fmt::Display>) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn extracts_method_hints_without_claiming_verification() {
    let report = inspect_source(SOURCE).unwrap();
    let names: Vec<&str> = report.methods.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["submit", "receipt", "cancel"]);
    assert_eq!(report.methods[0].parameters, ["self", "params"]);
    assert!(
        report.methods[0]
            .literal_dict_keys
            .contains(&"admission_id".to_owned())
    );
    assert!(!report.verified);
    assert!(!report.live_dispatch_enabled);
}

#[test]
fn detects_missing_or_duplicate_handlers() {
    assert!(message(inspect_source("def submit(): pass")).contains("ambiguous"));
    let duplicate = format!("{SOURCE}\ndef cancel(): pass\n");
    assert!(message(inspect_source(&duplicate)).contains("ambiguous"));
}

#[test]
fn rejects_unparseable_input() {
    assert!(message(inspect_source("def submit(??")).contains("Cannot parse"));
}

#[test]
fn checkout_reads_only_expected_source() {
    let tmp = tempfile::tempdir().unwrap();
    let gateway = tmp.path().join("gateway");
    std::fs::create_dir(&gateway).unwrap();
    std::fs::write(gateway.join("session_controls.py"), SOURCE).unwrap();
    let report = inspect_checkout(tmp.path()).unwrap();
    assert_eq!(report.source, "gateway/session_controls.py");
    assert_eq!(
        report.methods[0].literal_dict_keys,
        ["admission_id", "state"]
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlink_source() {
    let tmp = tempfile::tempdir().unwrap();
    let gateway = tmp.path().join("gateway");
    std::fs::create_dir(&gateway).unwrap();
    let target = tmp.path().join("outside.py");
    std::fs::write(&target, SOURCE).unwrap();
    std::os::unix::fs::symlink(&target, gateway.join("session_controls.py")).unwrap();
    assert!(message(inspect_checkout(tmp.path())).contains("symlinked"));
}
