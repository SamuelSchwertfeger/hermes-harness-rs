#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
use hermes_harness::{
    native::verify_applied::verify_applied,
    util::{resolve_strict, sha256_hex},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
const CONTENT: &str = "def add(a: int, b: int) -> int:\n    return a + b";
fn git(root: &Path, args: &[&str]) -> String {
    let r = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    String::from_utf8(r.stdout).unwrap().trim().to_owned()
}
struct Case {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    worktree: PathBuf,
    receipt: PathBuf,
    evidence: PathBuf,
}
impl Case {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("source");
        fs::create_dir(&root).unwrap();
        let root = resolve_strict(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "test@example.invalid"]);
        git(&root, &["config", "user.name", "Harness test"]);
        git(&root, &["config", "core.autocrlf", "false"]);
        fs::write(root.join("base.txt"), "original\n").unwrap();
        git(&root, &["add", "base.txt"]);
        git(&root, &["commit", "-qm", "baseline"]);
        let head = git(&root, &["rev-parse", "HEAD"]);
        let worktree = tmp.path().join("worktree");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "harness/test-audit",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::write(worktree.join("smoke_math.py"), CONTENT).unwrap();
        let receipt = tmp.path().join("receipt.json");
        let diff = "--- /dev/null\n+++ b/smoke_math.py\n@@ -0,0 +1,2 @@\n+def add(a: int, b: int) -> int:\n+    return a + b";
        fs::write(&receipt,serde_json::to_vec(&json!({"repository":root.to_string_lossy(),"mode":"live-reviewed-proposal-only","verdict":"accept","applied":false,"head":head,"task_id":"test-audit","files":[{"path":"smoke_math.py","sha256":sha256_hex(CONTENT.as_bytes()),"diff":diff}]})).unwrap()).unwrap();
        let evidence = tmp.path().join("verification.json");
        Self {
            _tmp: tmp,
            root,
            worktree,
            receipt,
            evidence,
        }
    }
    fn run(
        &self,
        grant: bool,
    ) -> Result<Value, hermes_harness::multiagent::runtime::WorkflowError> {
        verify_applied(
            &self.root,
            &self.receipt,
            &self.worktree,
            &self.evidence,
            grant,
        )
    }
}
#[test]
fn successful_integrity_audit() {
    let c = Case::new();
    let r = c.run(true).unwrap();
    assert_eq!(r["integrity_verified"], true);
    assert_eq!(r["changed_paths"], json!(["smoke_math.py"]));
    assert_eq!(r["generated_code_executed"], false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(&c.evidence).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let evidence: Value = serde_json::from_slice(&fs::read(&c.evidence).unwrap()).unwrap();
    assert_eq!(evidence["human_decision_pending"], true);
}
#[test]
fn requires_separate_grant() {
    let c = Case::new();
    assert!(
        c.run(false)
            .unwrap_err()
            .to_string()
            .contains("authorization")
    );
    assert!(!c.evidence.exists());
}
#[test]
fn rejects_modified_file() {
    let c = Case::new();
    fs::write(
        c.worktree.join("smoke_math.py"),
        format!("{CONTENT}\n# changed"),
    )
    .unwrap();
    assert!(
        c.run(true)
            .unwrap_err()
            .to_string()
            .contains("content mismatch")
    );
    assert!(!c.evidence.exists());
}
#[test]
fn rejects_extra_path() {
    let c = Case::new();
    fs::write(c.worktree.join("other.py"), "extra").unwrap();
    assert!(
        c.run(true)
            .unwrap_err()
            .to_string()
            .contains("outside ownership")
    );
    assert!(!c.evidence.exists());
}
#[test]
fn rejects_reused_evidence() {
    let c = Case::new();
    c.run(true).unwrap();
    assert!(
        c.run(true)
            .unwrap_err()
            .to_string()
            .contains("already exists")
    );
}
#[test]
fn rejects_different_baseline() {
    let c = Case::new();
    let mut data: Value = serde_json::from_slice(&fs::read(&c.receipt).unwrap()).unwrap();
    data["head"] = json!("a".repeat(40));
    fs::write(&c.receipt, serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(
        c.run(true)
            .unwrap_err()
            .to_string()
            .contains("baseline changed")
    );
    assert!(!c.evidence.exists());
}
