#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Restricted disposable worktree proposal-application tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::multiagent::apply_edits::apply_new_files;
use hermes_harness::multiagent::model_edits::parse_edit_proposal;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn worktree(tmp: &Path) -> (PathBuf, String) {
    let repo = tmp.join("repo");
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
    let workspace = tmp.join("workspace");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "test",
            workspace.to_str().unwrap(),
            "HEAD",
        ],
    );
    let head = git(&workspace, &["rev-parse", "HEAD"]);
    (workspace, head)
}

fn proposal() -> &'static str {
    r#"{"task_id":"a","files":[{"path":"tests/new.py","content":"assert True\n"}]}"#
}

fn scopes() -> Vec<String> {
    vec!["tests".to_owned()]
}

#[test]
fn creates_only_scoped_new_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, baseline) = worktree(tmp.path());
    let parsed = parse_edit_proposal(proposal(), "a", &scopes()).unwrap();
    let result = apply_new_files(&workspace, &parsed, &scopes(), &baseline, true).unwrap();
    assert_eq!(result.audit.changed_paths, vec!["tests/new.py".to_owned()]);
    assert_eq!(
        std::fs::read_to_string(workspace.join("tests/new.py")).unwrap(),
        "assert True\n"
    );
}

#[test]
fn requires_authorization() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, baseline) = worktree(tmp.path());
    let parsed = parse_edit_proposal(proposal(), "a", &scopes()).unwrap();
    let error = apply_new_files(&workspace, &parsed, &scopes(), &baseline, false).unwrap_err();
    assert!(error.to_string().contains("authorization"));
    assert!(!workspace.join("tests/new.py").exists());
}

#[test]
fn refuses_existing_content() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, baseline) = worktree(tmp.path());
    std::fs::create_dir(workspace.join("tests")).unwrap();
    std::fs::write(workspace.join("tests/new.py"), "existing").unwrap();
    let parsed = parse_edit_proposal(proposal(), "a", &scopes()).unwrap();
    let error = apply_new_files(&workspace, &parsed, &scopes(), &baseline, true).unwrap_err();
    assert!(error.to_string().contains("clean worktree"));
    assert_eq!(
        std::fs::read_to_string(workspace.join("tests/new.py")).unwrap(),
        "existing"
    );
}

#[cfg(unix)]
#[test]
fn refuses_symlink_ancestor() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, baseline) = worktree(tmp.path());
    std::os::unix::fs::symlink(tmp.path(), workspace.join("tests")).unwrap();
    let parsed = parse_edit_proposal(proposal(), "a", &scopes()).unwrap();
    assert!(apply_new_files(&workspace, &parsed, &scopes(), &baseline, true).is_err());
    assert!(!tmp.path().join("new.py").exists());
}
