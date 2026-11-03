//! Explicit live runtime qualification checklist; no self-attestation as proof.
//!
//! Readiness evidence must be supplied by the operator's independent host probe.
//! Passing this structural checklist alone never creates a runtime qualifier.

use crate::util::is_py_space;
use std::collections::{BTreeMap, BTreeSet};

/// Controls that must each have exactly one passing, evidenced finding.
pub const REQUIRED_CONTROLS: [&str; 11] = [
    "sandbox_identity",
    "network_egress",
    "secret_scope",
    "write_scope",
    "resource_limits",
    "gateway_authentication",
    "session_binding",
    "cancellation",
    "terminal_receipt",
    "worker_evidence",
    "reviewer_read_only",
];

/// Recorded operator assertion, not independently verified attestation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessFinding {
    /// Control name.
    pub control: String,
    /// Reference to the operator's evidence record.
    pub evidence_reference: String,
    /// Whether the operator asserts the control passed.
    pub passed: bool,
}

impl ReadinessFinding {
    /// Convenience constructor.
    #[must_use]
    pub fn new(control: &str, evidence_reference: &str, passed: bool) -> Self {
        Self {
            control: control.to_owned(),
            evidence_reference: evidence_reference.to_owned(),
            passed,
        }
    }
}

/// Coverage report; intentionally cannot authorize gateway execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessReport {
    /// Required controls with no finding, sorted.
    pub missing: Vec<String>,
    /// Controls with failed, duplicate, unknown or blank findings, sorted.
    pub failed: Vec<String>,
    /// True only when nothing is missing or failed.
    pub structurally_complete: bool,
}

/// Fail closed for missing, duplicate, failed or empty evidence records.
#[must_use]
pub fn assess_readiness(findings: &[ReadinessFinding]) -> ReadinessReport {
    let mut recorded: BTreeMap<&str, &ReadinessFinding> = BTreeMap::new();
    let mut failed: BTreeSet<String> = BTreeSet::new();
    for finding in findings {
        let control = finding.control.as_str();
        if recorded.contains_key(control) || !REQUIRED_CONTROLS.contains(&control) {
            failed.insert(finding.control.clone());
        }
        if !finding.passed
            || finding
                .evidence_reference
                .trim_matches(is_py_space)
                .is_empty()
        {
            failed.insert(finding.control.clone());
        }
        recorded.insert(control, finding);
    }
    let mut missing: Vec<String> = REQUIRED_CONTROLS
        .iter()
        .filter(|control| !recorded.contains_key(*control))
        .map(|control| (*control).to_owned())
        .collect();
    missing.sort();
    let failed: Vec<String> = failed.into_iter().collect();
    let structurally_complete = missing.is_empty() && failed.is_empty();
    ReadinessReport {
        missing,
        failed,
        structurally_complete,
    }
}
