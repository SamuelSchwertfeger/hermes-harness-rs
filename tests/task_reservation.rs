#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Offline crash and contention regressions on disposable Git fixtures.

use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::core::delegation::{Brief, DelegationError, Finding, OfflineAgent};
use hermes_harness::core::task_reservation::{
    RecoveryState, ReservationError, fence, reconcile, reserve,
};
use hermes_harness::git::worktree_delegation::{
    TaskWorkspace, WorktreeDelegationError, delegate_in_worktree, prepare_worktree, review_worktree,
};
use hermes_harness::git::worktrees::resolve_strict;

struct CountingAgent {
    identity: String,
    fail: bool,
    calls: u32,
}

impl CountingAgent {
    fn new(identity: &str, fail: bool) -> Self {
        Self {
            identity: identity.to_owned(),
            fail,
            calls: 0,
        }
    }
}

impl OfflineAgent for CountingAgent {
    fn run(
        &mut self,
        _brief: &Brief,
        _prior: Option<&Finding>,
    ) -> Result<Finding, DelegationError> {
        self.calls += 1;
        if self.fail {
            return Err(DelegationError::Agent("simulated worker crash".to_owned()));
        }
        Ok(Finding {
            agent_id: self.identity.clone(),
            text: "offline only".to_owned(),
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

fn text<T>(result: Result<T, impl std::fmt::Display>) -> String {
    match result {
        Ok(_) => panic!("expected error"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn reserved_crash_is_read_only() {
    let (_tmp, task, repo) = task_repo();
    let record = reserve(&task, &repo).unwrap();
    assert!(!record.baseline.is_empty());
    assert_eq!(reconcile(&task).unwrap(), RecoveryState::Reserved);
    assert!(!task.workspace.exists());
    let message = text(reserve(&task, &repo));
    assert!(message.contains("reserved"), "{message}");
}

#[test]
fn preparing_crash_never_retries() {
    let (_tmp, task, repo) = task_repo();
    reserve(&task, &repo).unwrap();
    fence(&task, "preparing").unwrap();
    assert_eq!(reconcile(&task).unwrap(), RecoveryState::Preparing);
    assert!(matches!(
        prepare_worktree(&task, &repo),
        Err(WorktreeDelegationError::Reservation(
            ReservationError::Delegation(_)
        ))
    ));
}

#[test]
fn uncertain_worker_must_not_repeat() {
    let (_tmp, task, repo) = task_repo();
    prepare_worktree(&task, &repo).unwrap();
    let mut agent = CountingAgent::new("worker", true);
    let message = text(delegate_in_worktree(&task, &mut agent));
    assert!(message.contains("crash"), "{message}");
    assert_eq!(reconcile(&task).unwrap(), RecoveryState::DispatchUncertain);
    let message = text(delegate_in_worktree(&task, &mut agent));
    assert!(message.contains("Dispatch requires"), "{message}");
    assert_eq!(agent.calls, 1);
}

#[test]
fn partial_worker_output_is_uncertain() {
    let (_tmp, task, repo) = task_repo();
    prepare_worktree(&task, &repo).unwrap();
    fence(&task, "dispatch").unwrap();
    std::fs::write(
        task.state_root
            .join(&task.brief.task_id)
            .join("worker.json"),
        "{}",
    )
    .unwrap();
    assert_eq!(reconcile(&task).unwrap(), RecoveryState::DispatchUncertain);
}

#[test]
fn mismatched_brief_rejected() {
    let (_tmp, task, repo) = task_repo();
    reserve(&task, &repo).unwrap();
    let mut changed = task;
    changed.brief.objective = "changed".to_owned();
    let message = text(reconcile(&changed));
    assert!(message.contains("digest"), "{message}");
}

#[cfg(unix)]
#[test]
fn symlinked_reservation_rejected() {
    let (_tmp, task, repo) = task_repo();
    reserve(&task, &repo).unwrap();
    let path = task
        .state_root
        .join(&task.brief.task_id)
        .join("reservation.json");
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(repo.join("README.md"), &path).unwrap();
    let message = text(reconcile(&task));
    assert!(message.contains("Symlink"), "{message}");
}

#[test]
fn malformed_stage_requires_manual_intervention() {
    let (_tmp, task, repo) = task_repo();
    reserve(&task, &repo).unwrap();
    std::fs::write(
        task.state_root
            .join(&task.brief.task_id)
            .join("preparing.json"),
        r#"{"stage": "wrong"}"#,
    )
    .unwrap();
    assert_eq!(reconcile(&task).unwrap(), RecoveryState::ManualIntervention);
}

#[test]
#[allow(clippy::needless_collect)] // all threads must spawn before any join
fn concurrent_reservers_only_one_wins() {
    let (_tmp, task, repo) = task_repo();
    let wins = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| match reserve(&task, &repo) {
                    Ok(_) => true,
                    Err(ReservationError::Delegation(_)) => false,
                    Err(other) => panic!("unexpected error: {other}"),
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|won| *won)
            .count()
    });
    assert_eq!(wins, 1);
    assert_eq!(reconcile(&task).unwrap(), RecoveryState::Reserved);
}

#[test]
fn prepared_and_worker_evidence() {
    let (_tmp, task, repo) = task_repo();
    prepare_worktree(&task, &repo).unwrap();
    assert_eq!(reconcile(&task).unwrap(), RecoveryState::Prepared);
    delegate_in_worktree(&task, &mut CountingAgent::new("worker", false)).unwrap();
    assert_eq!(reconcile(&task).unwrap(), RecoveryState::WorkerRecorded);
    review_worktree(&task, &mut CountingAgent::new("reviewer", false)).unwrap();
    let message = text(review_worktree(
        &task,
        &mut CountingAgent::new("reviewer", false),
    ));
    assert!(message.contains("Reviewer already"), "{message}");
}
