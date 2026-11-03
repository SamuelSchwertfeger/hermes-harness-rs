#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
use hermes_harness::native::stage_approvals::{STAGES, approve, begin, snapshot, submit};
use std::{fs, path::Path, process::Command};
fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}
#[test]
fn four_stage_checkpoint_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-qb", "main"]);
    fs::write(repo.join("README.md"), "hello").unwrap();
    git(&repo, &["add", "README.md"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "init",
        ],
    );
    let state = tmp.path().join("state");
    fs::create_dir(&state).unwrap();
    let evidence = tmp.path().join("evidence.json");
    fs::write(&evidence, r#"{"verified":true}"#).unwrap();
    assert_eq!(
        begin(&state, "ticket-1", &repo, "hermes-session").unwrap()["next_stage"],
        "lets-investigate"
    );
    assert!(
        submit(
            &state,
            "ticket-1",
            "agents-assemble",
            &evidence,
            "hermes-session"
        )
        .unwrap_err()
        .to_string()
        .contains("out of order")
    );
    for stage in STAGES {
        let pending = submit(&state, "ticket-1", stage, &evidence, "hermes-session").unwrap();
        assert_eq!(pending["approval_pending"], true);
        let digest = pending["evidence"]["sha256"].as_str().unwrap();
        assert!(
            approve(&state, "ticket-1", stage, digest, "hermes-session", false)
                .unwrap_err()
                .to_string()
                .contains("operator")
        );
        assert!(
            approve(&state, "ticket-1", stage, "wrong", "hermes-session", true)
                .unwrap_err()
                .to_string()
                .contains("digest")
        );
        approve(&state, "ticket-1", stage, digest, "hermes-session", true).unwrap();
    }
    assert_eq!(snapshot(&state, "ticket-1").unwrap()["finished"], true);
    assert!(
        submit(
            &state,
            "ticket-1",
            "lets-investigate",
            &evidence,
            "hermes-session"
        )
        .unwrap_err()
        .to_string()
        .contains("out of order")
    );
}
