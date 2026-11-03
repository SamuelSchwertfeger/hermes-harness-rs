//! Fail-closed checklist coverage; not a live qualification test.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]

use std::collections::BTreeSet;

use hermes_harness::services::live_runtime_readiness::{
    REQUIRED_CONTROLS, ReadinessFinding, assess_readiness,
};

#[test]
fn missing_controls_fail_closed() {
    let report = assess_readiness(&[]);
    assert!(!report.structurally_complete);
    let missing: BTreeSet<&str> = report.missing.iter().map(String::as_str).collect();
    assert_eq!(missing, REQUIRED_CONTROLS.into_iter().collect());
}

#[test]
fn complete_report_is_not_a_runtime_attestation() {
    let findings: Vec<ReadinessFinding> = REQUIRED_CONTROLS
        .iter()
        .map(|control| ReadinessFinding::new(control, "host-probe-record", true))
        .collect();
    let report = assess_readiness(&findings);
    assert!(report.structurally_complete);
    assert!(report.failed.is_empty());
}

#[test]
fn duplicate_or_blank_evidence_fails() {
    let findings = [
        ReadinessFinding::new("cancellation", "", true),
        ReadinessFinding::new("cancellation", "x", true),
    ];
    assert!(
        assess_readiness(&findings)
            .failed
            .contains(&"cancellation".to_owned())
    );
}
