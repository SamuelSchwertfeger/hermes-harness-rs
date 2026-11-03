#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Sandbox verification authorization and non-execution regressions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::multiagent::runtime::WorkflowError;
use hermes_harness::native::functional_verify::{
    SandboxRun, SystemHost, VerifyHost, audit_checkout, functional_verify, functional_verify_with,
    sandbox_argv,
};
use hermes_harness::util::{resolve_strict, sha256_hex};
use serde_json::{Map, Value, json};

struct FakeHost {
    bwrap: Option<PathBuf>,
    audit_stub: Option<BTreeMap<String, String>>,
    stderr: Vec<u8>,
    code: i32,
}

impl FakeHost {
    fn with_bwrap() -> Self {
        Self {
            bwrap: Some(PathBuf::from("/usr/bin/bwrap")),
            audit_stub: None,
            stderr: Vec::new(),
            code: 0,
        }
    }
}

impl VerifyHost for FakeHost {
    fn find_bwrap(&self) -> Option<PathBuf> {
        self.bwrap.clone()
    }

    fn python_available(&self, python: &str) -> bool {
        python == "/usr/bin/python3"
    }

    fn audit(
        &self,
        root: &Path,
        record: &Map<String, Value>,
    ) -> Result<BTreeMap<String, String>, WorkflowError> {
        self.audit_stub
            .as_ref()
            .map_or_else(|| SystemHost.audit(root, record), |stub| Ok(stub.clone()))
    }

    fn run(&self, _argv: &[String]) -> Result<SandboxRun, WorkflowError> {
        Ok(SandboxRun {
            code: Some(self.code),
            timed_out: false,
            stdout: Vec::new(),
            stderr: self.stderr.clone(),
        })
    }
}

fn git(workspace: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn checkpoint(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let tmp = resolve_strict(tmp).unwrap();
    let workspace = tmp.join("worktree");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(workspace.join("tests")).unwrap();
    std::fs::write(workspace.join("tests/test_example.py"), "import unittest\n").unwrap();
    let init = Command::new("git")
        .args(["init", "-q"])
        .arg(&workspace)
        .status()
        .unwrap();
    assert!(init.success());
    git(&workspace, &["add", "tests/test_example.py"]);
    git(&workspace, &["commit", "-qm", "baseline"]);
    let output = Command::new("git")
        .arg("-C")
        .arg(&workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let baseline = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    git(&workspace, &["switch", "-qc", "harness/test-one"]);
    std::fs::write(workspace.join("reviewed.py"), "VALUE = 7\n").unwrap();
    let checkpoint = tmp.join("checkpoint.json");
    let record = json!({
        "mode": "integrity-checkpoint",
        "integrity_verified": true,
        "functional_verified": false,
        "tests_executed": false,
        "human_decision_pending": true,
        "next_stage": "requires-separate-functional-execution-authorization",
        "task_id": "test-one",
        "baseline": baseline,
        "branch": "harness/test-one",
        "workspace": workspace.to_string_lossy(),
        "files": [{"path": "reviewed.py", "sha256": sha256_hex(b"VALUE = 7\n")}],
    });
    std::fs::write(&checkpoint, serde_json::to_string(&record).unwrap()).unwrap();
    (workspace, checkpoint, tmp.join("result.json"))
}

fn verify(
    host: &FakeHost,
    fixture: &(PathBuf, PathBuf, PathBuf),
    authorized: bool,
) -> Result<Value, WorkflowError> {
    let (workspace, checkpoint, evidence) = fixture;
    functional_verify_with(host, checkpoint, workspace, evidence, authorized)
}

fn expect_error(result: Result<Value, WorkflowError>, expected: &str) {
    let error = result.unwrap_err().to_string();
    assert!(error.contains(expected), "{error:?} lacks {expected:?}");
}

fn load_record(checkpoint: &Path) -> Map<String, Value> {
    match serde_json::from_slice(&std::fs::read(checkpoint).unwrap()).unwrap() {
        Value::Object(map) => map,
        _ => panic!("checkpoint is not an object"),
    }
}

#[test]
fn rejects_without_separate_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, checkpoint, evidence) = checkpoint(tmp.path());
    let error = functional_verify(&checkpoint, &workspace, &evidence, false).unwrap_err();
    assert!(error.to_string().contains("authorization"));
    assert!(!evidence.exists());
}

#[test]
fn rejects_missing_bwrap() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = checkpoint(tmp.path());
    let host = FakeHost {
        bwrap: None,
        ..FakeHost::with_bwrap()
    };
    expect_error(verify(&host, &fixture, true), "Bubblewrap");
    assert!(!fixture.2.exists());
}

#[test]
fn rejects_tampered_reviewed_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = checkpoint(tmp.path());
    std::fs::write(fixture.0.join("reviewed.py"), "VALUE = 99\n").unwrap();
    expect_error(
        verify(&FakeHost::with_bwrap(), &fixture, true),
        "changed since checkpoint",
    );
    assert!(!fixture.2.exists());
}

#[test]
fn sandbox_command_has_no_shell_and_no_host_home() {
    let tmp = tempfile::tempdir().unwrap();
    let command = sandbox_argv("/usr/bin/bwrap", tmp.path(), "/usr/bin/python3");
    let has = |item: &str| command.iter().any(|entry| entry == item);
    assert!(has("--unshare-all"));
    assert!(has("--clearenv"));
    assert!(has("--ro-bind"));
    assert!(has("--die-with-parent"));
    assert!(
        command
            .windows(3)
            .any(|args| args == ["/usr/bin/python3", "-I", "-m"])
    );
    assert!(has("-m"));
    assert!(has("unittest"));
    assert!(!has("/home"));
    assert!(!has("sh"));
}

#[test]
fn success_records_bounded_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = checkpoint(tmp.path());
    let record = load_record(&fixture.1);
    audit_checkout(&fixture.0, &record).unwrap();
    let host = FakeHost {
        audit_stub: Some(BTreeMap::new()),
        stderr: b"Ran 1 test in 0.001s\n\nOK\n".to_vec(),
        ..FakeHost::with_bwrap()
    };
    let output = verify(&host, &fixture, true).unwrap();
    assert_eq!(output["functional_verified"], json!(true));
    assert_eq!(output["next_stage"], json!("await-human-review"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&fixture.2).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let saved: Value = serde_json::from_slice(&std::fs::read(&fixture.2).unwrap()).unwrap();
    assert_eq!(saved["human_decision_pending"], json!(true));
}

#[test]
fn no_test_directory_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = checkpoint(tmp.path());
    std::fs::remove_file(fixture.0.join("tests/test_example.py")).unwrap();
    std::fs::remove_dir(fixture.0.join("tests")).unwrap();
    expect_error(
        verify(&FakeHost::with_bwrap(), &fixture, true),
        "tests directory",
    );
}

#[test]
fn failing_tests_are_recorded_as_executed() {
    let cases: [(i32, &[u8], bool, &str); 3] = [
        (
            1,
            b"Ran 1 test in 0.001s\n\nFAILED (failures=1)\n",
            true,
            "verification-failed-requires-review",
        ),
        (
            1,
            b"bwrap: Creating new namespace failed: Operation not permitted\n",
            false,
            "execution-unconfirmed-requires-review",
        ),
        (
            1,
            b"python3: failed to start\n",
            false,
            "execution-unconfirmed-requires-review",
        ),
    ];
    for (code, stderr, expected_executed, expected_stage) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = checkpoint(tmp.path());
        let record = load_record(&fixture.1);
        audit_checkout(&fixture.0, &record).unwrap();
        let host = FakeHost {
            audit_stub: Some(BTreeMap::new()),
            stderr: stderr.to_vec(),
            code,
            ..FakeHost::with_bwrap()
        };
        let outcome = verify(&host, &fixture, true).unwrap();
        assert_eq!(outcome["tests_executed"], json!(expected_executed));
        assert_eq!(outcome["generated_code_executed"], json!(expected_executed));
        assert_eq!(outcome["functional_verified"], json!(false));
        assert_eq!(outcome["next_stage"], json!(expected_stage));
        let saved: Value = serde_json::from_slice(&std::fs::read(&fixture.2).unwrap()).unwrap();
        assert_eq!(saved["tests_executed"], json!(expected_executed));
    }
}

#[test]
fn rejects_unreviewed_test_file() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = checkpoint(tmp.path());
    std::fs::write(
        fixture.0.join("tests/test_injected.py"),
        "raise RuntimeError(\"unreviewed\")\n",
    )
    .unwrap();
    let error = verify(&FakeHost::with_bwrap(), &fixture, true)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("audit") || error.contains("changed paths"),
        "{error}"
    );
    assert!(!fixture.2.exists());
}

#[test]
fn rejects_changed_git_head() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = checkpoint(tmp.path());
    git(&fixture.0, &["commit", "-qm", "extra", "--allow-empty"]);
    expect_error(
        verify(&FakeHost::with_bwrap(), &fixture, true),
        "identity changed",
    );
    assert!(!fixture.2.exists());
}

#[test]
fn rejects_wrong_git_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = checkpoint(tmp.path());
    git(&fixture.0, &["switch", "-qc", "other-branch"]);
    expect_error(
        verify(&FakeHost::with_bwrap(), &fixture, true),
        "identity changed",
    );
    assert!(!fixture.2.exists());
}

#[test]
fn rejects_ignored_test_module() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = checkpoint(tmp.path());
    std::fs::write(
        fixture.0.join(".git/info/exclude"),
        "tests/test_ignored.py\n",
    )
    .unwrap();
    std::fs::write(
        fixture.0.join("tests/test_ignored.py"),
        "raise RuntimeError(\"ignored\")\n",
    )
    .unwrap();
    expect_error(
        verify(&FakeHost::with_bwrap(), &fixture, true),
        "ignored Python test",
    );
    assert!(!fixture.2.exists());
}

#[test]
fn rejects_ignored_python_startup_files() {
    for name in [
        "sitecustomize.py",
        "usercustomize.py",
        "startup.pth",
        "tests/nested.pth",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = checkpoint(tmp.path());
        std::fs::write(fixture.0.join(".git/info/exclude"), format!("{name}\n")).unwrap();
        std::fs::write(fixture.0.join(name), "import os\n").unwrap();
        expect_error(
            verify(&FakeHost::with_bwrap(), &fixture, true),
            "ignored Python test",
        );
        assert!(!fixture.2.exists());
    }
}
