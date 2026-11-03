//! Trusted passing/failing fixtures through the existing Bubblewrap gate.
use super::ProbeError;
use serde_json::Value;

/// Run both trusted fixtures; fail closed on platforms without the Unix gate.
pub fn run() -> Result<Value, ProbeError> {
    platform_run()
}

#[cfg(not(unix))]
fn platform_run() -> Result<Value, ProbeError> {
    Err(ProbeError::runtime(
        "Functional fixture requires Unix and Bubblewrap",
    ))
}

#[cfg(unix)]
#[allow(clippy::too_many_lines)] // Keep the complete trusted pass/fail matrix together.
fn platform_run() -> Result<Value, ProbeError> {
    use super::unknown_recovery::Scratch;
    use crate::native::functional_verify::functional_verify;
    use serde_json::json;
    use sha2::{Digest as _, Sha256};
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::process::Command;

    fn git(workspace: &Path, args: &[&str]) -> Result<String, ProbeError> {
        let output = Command::new("/usr/bin/git")
            .current_dir(workspace)
            .args(args)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()?;
        if !output.status.success() {
            return Err(ProbeError::runtime("Trusted fixture Git setup failed"));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().into())
    }
    fn check(condition: bool, message: &str) -> Result<(), ProbeError> {
        if condition {
            Ok(())
        } else {
            Err(ProbeError::runtime(message))
        }
    }

    let base = Scratch::new("hermes-trusted-functional")?;
    let mut reports = Vec::new();
    for (label, sum, expected) in [("passing", 4, true), ("failing", 5, false)] {
        let workspace = base.0.join(label);
        std::fs::create_dir_all(workspace.join("tests"))?;
        // functional_verify compares against the resolved path, as Python's resolve().
        let workspace = workspace.canonicalize()?;
        let file = workspace.join("tests/test_trusted_fixture.py");
        std::fs::write(
            &file,
            format!(
                "import unittest\n\nclass TrustedFixture(unittest.TestCase):\n    def test_arithmetic(self):\n        self.assertEqual(2 + 2, {sum})\n"
            ),
        )?;
        git(&workspace, &["init", "-q"])?;
        git(&workspace, &["commit", "--allow-empty", "-qm", "baseline"])?;
        let baseline = git(&workspace, &["rev-parse", "HEAD"])?;
        let task_id = format!("trusted-fixture-{label}");
        let branch = format!("harness/{task_id}");
        git(&workspace, &["switch", "-qc", &branch])?;
        let checkpoint = base.0.join(format!("{label}-checkpoint.json"));
        let evidence = base.0.join(format!("{label}-evidence.json"));
        let record = json!({
            "mode":"integrity-checkpoint", "integrity_verified":true,
            "functional_verified":false, "tests_executed":false, "human_decision_pending":true,
            "workspace":workspace, "next_stage":"requires-separate-functional-execution-authorization",
            "files":[{"path":"tests/test_trusted_fixture.py", "sha256":hex::encode(Sha256::digest(std::fs::read(&file)?))}],
            "task_id":task_id, "branch":branch, "baseline":baseline
        });
        std::fs::write(&checkpoint, record.to_string())?;
        let result = functional_verify(&checkpoint, &workspace, &evidence, true)
            .map_err(ProbeError::runtime)?;
        let saved: Value =
            serde_json::from_slice(&std::fs::read(&evidence)?).map_err(ProbeError::runtime)?;
        for key in [
            "sandbox_started",
            "tests_executed",
            "generated_code_executed",
        ] {
            check(
                result.get(key) == Some(&Value::Bool(true)),
                "Fixture execution not recorded",
            )?;
        }
        check(
            result.get("functional_verified") == Some(&Value::Bool(expected)),
            "Unexpected fixture verdict",
        )?;
        check(
            result.get("timed_out") == Some(&Value::Bool(false)),
            "Fixture timed out",
        )?;
        check(
            std::fs::metadata(&evidence)?.permissions().mode() & 0o777 == 0o600,
            "Wrong permissions",
        )?;
        for key in ["functional_verified", "tests_executed"] {
            check(saved.get(key) == result.get(key), "Saved evidence mismatch")?;
        }
        let digest = hex::encode(Sha256::digest(std::fs::read(&checkpoint)?));
        check(
            saved.get("checkpoint_sha256").and_then(Value::as_str) == Some(&digest),
            "Checkpoint digest mismatch",
        )?;
        reports
            .push(json!({"fixture":label, "functional_verified":expected, "tests_executed":true}));
    }
    Ok(json!({"result":"verified", "fixtures":reports, "evidence_directory":base.0}))
}
