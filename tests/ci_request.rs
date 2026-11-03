#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline immutable-commit CI request tests; never connects to CI providers.

use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::ci::CiError;
use hermes_harness::ci::ci_request::{create_ci_request, inspect_ci_request};
use hermes_harness::core::delegation::{Brief, DelegationError, Finding, OfflineAgent};
use hermes_harness::git::worktree_delegation::{
    TaskWorkspace, delegate_in_worktree, prepare_worktree, review_worktree,
};
use hermes_harness::git::worktrees::{git as hgit, resolve_strict};
use serde_json::Value;

struct CountingAgent {
    identity: String,
}

impl OfflineAgent for CountingAgent {
    fn run(
        &mut self,
        _brief: &Brief,
        _prior: Option<&Finding>,
    ) -> Result<Finding, DelegationError> {
        Ok(Finding {
            agent_id: self.identity.clone(),
            text: "offline only".to_owned(),
        })
    }
}

struct Worker {
    workspace: PathBuf,
}

impl OfflineAgent for Worker {
    fn run(
        &mut self,
        _brief: &Brief,
        _prior: Option<&Finding>,
    ) -> Result<Finding, DelegationError> {
        let path = self.workspace.join("src").join("safe.py");
        std::fs::create_dir(path.parent().unwrap())?;
        std::fs::write(&path, "reviewed candidate\n")?;
        Ok(Finding {
            agent_id: "worker".to_owned(),
            text: "offline work".to_owned(),
        })
    }
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
}

fn task_repo() -> (tempfile::TempDir, TaskWorkspace, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let base = resolve_strict(tmp.path()).unwrap();
    let repo = base.join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "local@example.invalid"],
        vec!["config", "user.name", "Offline"],
        vec!["config", "core.autocrlf", "false"],
    ] {
        git(&repo, &args);
    }
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "baseline"]);
    let state = base.join("state");
    std::fs::create_dir(&state).unwrap();
    let task = TaskWorkspace {
        state_root: state,
        workspace: base.join("workspace"),
        brief: Brief {
            task_id: "task-one".to_owned(),
            objective: "test".to_owned(),
            owned_paths: vec!["src/safe.py".to_owned()],
            worker_id: "worker".to_owned(),
            reviewer_id: "reviewer".to_owned(),
        },
    };
    (tmp, task, repo)
}

fn reviewed() -> (tempfile::TempDir, TaskWorkspace) {
    let (tmp, task, repo) = task_repo();
    prepare_worktree(&task, &repo).unwrap();
    let mut worker = Worker {
        workspace: task.workspace.clone(),
    };
    delegate_in_worktree(&task, &mut worker).unwrap();
    let mut reviewer = CountingAgent {
        identity: "reviewer".to_owned(),
    };
    review_worktree(&task, &mut reviewer).unwrap();
    (tmp, task)
}

fn commit_all(task: &TaskWorkspace, message: &str) {
    hgit(&task.workspace, &["add", "src/safe.py"]).unwrap();
    commit(task, &["commit", "-m", message]);
}

fn commit(task: &TaskWorkspace, tail: &[&str]) {
    let mut args = vec![
        "-c",
        "user.name=Offline",
        "-c",
        "user.email=offline@example.invalid",
    ];
    args.extend_from_slice(tail);
    hgit(&task.workspace, &args).unwrap();
}

fn text<T>(result: Result<T, impl std::fmt::Display>) -> String {
    match result {
        Ok(_) => panic!("expected error"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn request_requires_clean_commit() {
    let (_tmp, task) = reviewed();
    let message = text(create_ci_request(&task));
    assert!(message.contains("committed and clean"), "{message}");
    commit_all(&task, "offline candidate");
    let request = create_ci_request(&task).unwrap();
    assert_eq!(
        request.commit,
        hgit(&task.workspace, &["rev-parse", "HEAD"]).unwrap()
    );
    assert_eq!(
        request.tree,
        hgit(&task.workspace, &["rev-parse", "HEAD^{tree}"]).unwrap()
    );
    assert_eq!(inspect_ci_request(&task).unwrap(), request);
    match create_ci_request(&task) {
        Err(CiError::Delegation(DelegationError::Io(err))) => {
            assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        }
        other => panic!("expected FileExistsError, got {other:?}"),
    }
}

#[test]
fn request_rejects_changed_content() {
    let (_tmp, task) = reviewed();
    std::fs::write(task.workspace.join("src/safe.py"), "tampered\n").unwrap();
    commit_all(&task, "tampered candidate");
    let message = text(create_ci_request(&task));
    assert!(message.contains("snapshot"), "{message}");
}

#[test]
fn request_is_not_ci_result_and_detects_tampering() {
    let (_tmp, task) = reviewed();
    commit_all(&task, "offline candidate");
    create_ci_request(&task).unwrap();
    let path = task
        .state_root
        .join(&task.brief.task_id)
        .join("ci-request.json");
    let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(value.get("passed").is_none());
    assert!(value.get("approved").is_none());
    value["candidate"]["tree"] = Value::String("0".repeat(40));
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
    let message = text(inspect_ci_request(&task));
    assert!(message.contains("checksum"), "{message}");
}

#[test]
fn candidate_refuses_head_change() {
    let (_tmp, task) = reviewed();
    commit_all(&task, "offline candidate");
    create_ci_request(&task).unwrap();
    commit(&task, &["commit", "--allow-empty", "-m", "new head"]);
    let message = text(inspect_ci_request(&task));
    assert!(message.contains("no longer matches"), "{message}");
}
