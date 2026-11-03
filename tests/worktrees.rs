#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Tests for isolated worktrees and enforceable Git path ownership.

use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::git::worktrees::{
    WorktreeError, WorktreeSpec, audit_worktree, create_worktree, remove_worktree, resolve_strict,
};

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn spec(ticket: &str, branch: &str, paths: &[&str]) -> WorktreeSpec {
    WorktreeSpec {
        ticket_id: ticket.to_owned(),
        branch: branch.to_owned(),
        owned_paths: paths.iter().map(|p| (*p).to_owned()).collect(),
    }
}

fn repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let base = resolve_strict(tmp.path()).unwrap();
    let source = base.join("source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-b", "main"]);
    git(&source, &["config", "user.email", "tests@example.invalid"]);
    git(&source, &["config", "user.name", "Harness Test"]);
    git(&source, &["config", "core.autocrlf", "false"]);
    std::fs::create_dir(source.join("src")).unwrap();
    std::fs::write(source.join("src").join("app.py"), "original\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "Initial"]);
    (tmp, source)
}

#[test]
fn isolation_and_audit() {
    let (_tmp, repo) = repo();
    let workspace = repo.parent().unwrap().join("ticket-a");
    let spec = spec("ticket-a", "harness/ticket-a", &["src/app.py"]);
    create_worktree(&repo, &workspace, &spec).unwrap();
    let baseline = git(&workspace, &["rev-parse", "HEAD"]);
    std::fs::write(workspace.join("src").join("app.py"), "changed\n").unwrap();
    let audit = audit_worktree(&workspace, &baseline, &spec.owned_paths).unwrap();
    assert_eq!(audit.changed_paths, vec!["src/app.py".to_owned()]);
    assert_eq!(
        std::fs::read_to_string(repo.join("src").join("app.py")).unwrap(),
        "original\n"
    );
    let err = remove_worktree(&repo, &workspace, &spec.branch).unwrap_err();
    assert!(matches!(err, WorktreeError::Dirty(_)));
    let text = err.to_string();
    assert!(text.contains("dirty"), "{text}");
    std::fs::write(workspace.join("src").join("app.py"), "original\n").unwrap();
    remove_worktree(&repo, &workspace, &spec.branch).unwrap();
}

#[test]
fn unowned_and_untracked_rejected() {
    let (_tmp, repo) = repo();
    let workspace = repo.parent().unwrap().join("ticket-b");
    let spec = spec("ticket-b", "harness/ticket-b", &["src"]);
    create_worktree(&repo, &workspace, &spec).unwrap();
    let baseline = git(&workspace, &["rev-parse", "HEAD"]);
    std::fs::write(workspace.join("secret.txt"), "untracked\n").unwrap();
    let err = audit_worktree(&workspace, &baseline, &spec.owned_paths).unwrap_err();
    assert!(matches!(err, WorktreeError::Ownership(_)));
    let text = err.to_string();
    assert!(text.contains("outside ownership"), "{text}");
}

#[test]
fn existing_workspace_rejected() {
    let (_tmp, repo) = repo();
    let workspace = repo.parent().unwrap().join("ticket-c");
    let spec = spec("ticket-c", "harness/ticket-c", &["src"]);
    create_worktree(&repo, &workspace, &spec).unwrap();
    let err = create_worktree(&repo, &workspace, &spec).unwrap_err();
    assert!(matches!(err, WorktreeError::Workspace(_)));
    let text = err.to_string();
    assert!(text.contains("already exists"), "{text}");
}

#[cfg(unix)]
#[test]
fn symlink_escape_rejected() {
    let (_tmp, repo) = repo();
    let workspace = repo.parent().unwrap().join("ticket-c");
    let spec = spec("ticket-c", "harness/ticket-c", &["src"]);
    create_worktree(&repo, &workspace, &spec).unwrap();
    let baseline = git(&workspace, &["rev-parse", "HEAD"]);
    std::os::unix::fs::symlink("/etc/passwd", workspace.join("src").join("link")).unwrap();
    let err = audit_worktree(&workspace, &baseline, &spec.owned_paths).unwrap_err();
    assert!(matches!(err, WorktreeError::Ownership(_)));
    let text = err.to_string();
    assert!(text.contains("escapes worktree"), "{text}");
}

#[test]
fn bad_ticket_and_ownership() {
    let (_tmp, repo) = repo();
    let bad = repo.parent().unwrap().join("bad");
    let err =
        create_worktree(&repo, &bad, &spec("../bad", "harness/../bad", &["src"])).unwrap_err();
    assert!(matches!(err, WorktreeError::InvalidInput(_)));
    let text = err.to_string();
    assert!(text.contains("Invalid ticket"), "{text}");
    let err = create_worktree(&repo, &bad, &spec("ok", "harness/ok", &["../outside"])).unwrap_err();
    assert!(matches!(err, WorktreeError::InvalidInput(_)));
    let text = err.to_string();
    assert!(text.contains("Invalid relative path"), "{text}");
}
