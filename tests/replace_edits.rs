#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Content-addressed existing-file replacement in a temporary worktree.

use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::multiagent::replace_edits::{FileReplacement, apply_replacements};
use hermes_harness::util::sha256_hex;

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

fn checkout(tmp: &Path) -> (PathBuf, String) {
    let repo = tmp.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    std::fs::write(repo.join("value.py"), "old = 1\n").unwrap();
    git(&repo, &["add", "value.py"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
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
            "harness/change",
            workspace.to_str().unwrap(),
            "HEAD",
        ],
    );
    let head = git(&workspace, &["rev-parse", "HEAD"]);
    (workspace, head)
}

fn owned() -> Vec<String> {
    vec!["value.py".to_owned()]
}

#[test]
fn replaces_only_matching_preimage() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, baseline) = checkout(tmp.path());
    let replacement = FileReplacement {
        path: "value.py".to_owned(),
        expected_sha256: sha256_hex(b"old = 1\n"),
        content: "new = 2\n".to_owned(),
    };
    let audit = apply_replacements(&workspace, &[replacement], &owned(), &baseline, true).unwrap();
    assert_eq!(audit.changed_paths, vec!["value.py".to_owned()]);
    assert_eq!(
        std::fs::read_to_string(workspace.join("value.py")).unwrap(),
        "new = 2\n"
    );
}

#[test]
fn rejects_stale_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, baseline) = checkout(tmp.path());
    let replacement = FileReplacement {
        path: "value.py".to_owned(),
        expected_sha256: "a".repeat(64),
        content: "no".to_owned(),
    };
    let error =
        apply_replacements(&workspace, &[replacement], &owned(), &baseline, true).unwrap_err();
    assert!(error.to_string().contains("preimage"));
    assert_eq!(
        std::fs::read_to_string(workspace.join("value.py")).unwrap(),
        "old = 1\n"
    );
}

#[test]
fn requires_explicit_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, baseline) = checkout(tmp.path());
    let replacement = FileReplacement {
        path: "value.py".to_owned(),
        expected_sha256: "a".repeat(64),
        content: "no".to_owned(),
    };
    let error =
        apply_replacements(&workspace, &[replacement], &owned(), &baseline, false).unwrap_err();
    assert!(error.to_string().contains("authorization"));
}
