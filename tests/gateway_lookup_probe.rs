#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Offline tests for bounded Hermes source lookup inspection.

use hermes_harness::gateway::gateway_lookup_probe::{ScanLimits, inspect_tree};

#[test]
fn finds_submission_id_lookup_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("gateway.py"),
        "class Admissions:
    def find_by_submission_id(self, submission_id: str):
        return self.db.execute('SELECT * FROM admissions WHERE submission_id = ?', (submission_id,))
",
    )
    .unwrap();

    let report = inspect_tree(tmp.path(), None).unwrap();

    assert_eq!(report.files_scanned, 1);
    assert!(!report.capability_verified);
    assert!(report.findings.iter().any(|finding| {
        finding.symbol == "Admissions.find_by_submission_id"
            && finding.identifiers.contains(&"submission_id".to_owned())
    }));
}

#[test]
fn ignores_unrelated_functions() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("plain.py"),
        "def health():\n    return {\"ok\": True}\n",
    )
    .unwrap();
    let report = inspect_tree(tmp.path(), None).unwrap();
    assert!(report.findings.is_empty());
}

#[test]
fn reports_parse_failure_without_executing_source() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("broken.py"), "def find_by_submission_id(\n").unwrap();
    let report = inspect_tree(tmp.path(), None).unwrap();
    assert_eq!(report.parse_failures.len(), 1);
    assert_eq!(report.parse_failures[0].path, "broken.py");
}

#[cfg(unix)]
#[test]
fn skips_symlinked_python_files() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("outside_lookup_probe.py");
    std::fs::write(
        &target,
        "def find_by_submission_id(submission_id):\n    return submission_id\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&target, tmp.path().join("linked.py")).unwrap();
    let report = inspect_tree(tmp.path(), None).unwrap();
    assert_eq!(report.files_scanned, 0);
    assert!(report.findings.is_empty());
}

#[test]
fn enforces_file_limit() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("one.py"), "x = 1\n").unwrap();
    std::fs::write(tmp.path().join("two.py"), "y = 2\n").unwrap();
    let limits = ScanLimits {
        max_files: 1,
        ..ScanLimits::default()
    };
    let err = inspect_tree(tmp.path(), Some(limits)).unwrap_err();
    assert!(err.to_string().contains("file limit"));
}

#[test]
fn enforces_total_byte_limit() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("one.py"), "x = 12345\n").unwrap();
    let limits = ScanLimits {
        max_total_bytes: 1,
        ..ScanLimits::default()
    };
    let report = inspect_tree(tmp.path(), Some(limits)).unwrap();
    assert_eq!(report.files_scanned, 0);
    assert!(report.truncated);
}
