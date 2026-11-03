#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Verifier allowlist and authorization tests avoid executing model code.

use hermes_harness::multiagent::verifier_runner::{
    VerifyOptions, run_verification, verify_and_evaluate,
};

fn src() -> Vec<String> {
    vec!["src".to_owned()]
}

#[test]
fn verifier_requires_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let error = run_verification(
        dir.path(),
        &"a".repeat(40),
        &src(),
        "reviewer",
        "pytest",
        VerifyOptions::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("authorization"));
}

#[test]
fn verifier_rejects_unknown_command() {
    let dir = tempfile::tempdir().unwrap();
    let options = VerifyOptions {
        authorized: true,
        trusted_code: true,
        ..VerifyOptions::default()
    };
    let error = run_verification(
        dir.path(),
        &"a".repeat(40),
        &src(),
        "reviewer",
        "rm-all",
        options,
    )
    .unwrap_err();
    assert!(error.to_string().contains("Unknown verifier"));
}

#[cfg(unix)]
fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let finished = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(finished.status.success());
    String::from_utf8(finished.stdout)
        .unwrap()
        .trim()
        .to_owned()
}

#[cfg(unix)]
#[test]
fn allowlisted_pytest_binds_real_snapshot_evidence() {
    use hermes_harness::multiagent::verification_evidence::evaluate_test_evidence;
    use hermes_harness::multiagent::verification_manifest::prepare_verification;

    let on_path = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
    };
    if !on_path("uv") || !on_path("pytest") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "baseline",
        ],
    );
    let worktree = dir.path().join("worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "harness/verify",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let baseline = git(&worktree, &["rev-parse", "HEAD"]);
    std::fs::write(
        worktree.join("test_demo.py"),
        "def test_ok():\n    assert 1 + 1 == 2\n",
    )
    .unwrap();
    let owned = vec!["test_demo.py".to_owned()];
    let manifest = prepare_verification(&worktree, &baseline, &owned).unwrap();
    let options = VerifyOptions {
        authorized: true,
        trusted_code: true,
        timeout_seconds: 45,
    };
    let run = run_verification(
        &worktree,
        &baseline,
        &owned,
        "test-verifier",
        "pytest",
        options,
    )
    .unwrap();
    assert_eq!(run.evidence.exit_code, 0, "{}", run.stderr);
    assert!(
        evaluate_test_evidence(&manifest, &run.evidence)
            .unwrap()
            .accepted
    );
    assert!(!repo.join("test_demo.py").exists());
}

#[test]
fn untrusted_code_never_executes_even_when_authorized() {
    let dir = tempfile::tempdir().unwrap();
    let options = VerifyOptions {
        authorized: true,
        trusted_code: false,
        ..VerifyOptions::default()
    };
    let error = run_verification(
        dir.path(),
        &"a".repeat(40),
        &src(),
        "verifier",
        "pytest",
        options,
    )
    .unwrap_err();
    assert!(error.to_string().contains("qualified isolated verifier"));
}

#[test]
fn wrapper_rejects_untrusted_execution() {
    let dir = tempfile::tempdir().unwrap();
    let options = VerifyOptions {
        authorized: true,
        trusted_code: false,
        ..VerifyOptions::default()
    };
    let error =
        verify_and_evaluate(dir.path(), &"a".repeat(40), &src(), "verifier", options).unwrap_err();
    assert!(error.to_string().contains("qualified isolated verifier"));
}
