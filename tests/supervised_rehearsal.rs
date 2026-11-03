#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! End-to-end supervised rehearsal with real disposable Git worktrees.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::agents::supervised_agents::SupervisedPolicy;
use hermes_harness::agents::supervised_rehearsal::{
    RehearsalContext, RehearsalError, run_supervised_rehearsal,
};
use hermes_harness::core::delegation::{Brief, DelegationError, Finding, OfflineAgent};
use hermes_harness::git::worktree_delegation::TaskWorkspace;
use hermes_harness::supervised::offline_orchestrator::{
    OfflineAgents, OrchestrationState, inspect_offline_task,
};
use hermes_harness::util::resolve_strict;

struct LocalDouble {
    identity: String,
    workspace: PathBuf,
    write: bool,
    fail: bool,
    calls: u32,
}

impl LocalDouble {
    fn new(identity: &str, workspace: &Path, write: bool) -> Self {
        Self {
            identity: identity.to_owned(),
            workspace: workspace.to_path_buf(),
            write,
            fail: false,
            calls: 0,
        }
    }
}

impl OfflineAgent for LocalDouble {
    fn run(&mut self, _brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        self.calls += 1;
        if self.fail {
            return Err(DelegationError::Agent(
                "simulated worker failure".to_owned(),
            ));
        }
        if self.write {
            let target = self.workspace.join("src").join("safe.py");
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, "offline fixture\n").unwrap();
        }
        Ok(Finding {
            agent_id: self.identity.clone(),
            text: if prior.is_some() { "review" } else { "work" }.to_owned(),
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

struct Setup {
    _tmp: tempfile::TempDir,
    task: TaskWorkspace,
    repo: PathBuf,
    policy: SupervisedPolicy,
}

fn setup() -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let base = resolve_strict(tmp.path()).unwrap();
    let repo = base.join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "test@example.invalid"],
        vec!["config", "user.name", "Tester"],
        vec!["config", "core.autocrlf", "false"],
    ] {
        git(&repo, &args);
    }
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "base"]);
    let root = base.join("state");
    std::fs::create_dir(&root).unwrap();
    let workspace = base.join("worktree");
    let brief = Brief {
        task_id: "rehearsal-one".to_owned(),
        objective: "Offline rehearsal".to_owned(),
        owned_paths: vec!["src/safe.py".to_owned()],
        worker_id: "worker".to_owned(),
        reviewer_id: "reviewer".to_owned(),
    };
    let policy = SupervisedPolicy::new(&brief.task_id, &workspace);
    Setup {
        _tmp: tmp,
        task: TaskWorkspace {
            state_root: root,
            workspace,
            brief,
        },
        repo,
        policy,
    }
}

#[test]
fn rehearsal_success_and_disabled_plan() {
    let s = setup();
    let worker = RefCell::new(LocalDouble::new("worker", &s.task.workspace, true));
    let reviewer = RefCell::new(LocalDouble::new("reviewer", &s.task.workspace, false));
    let agents = OfflineAgents::new(&worker, &reviewer);
    let context = RehearsalContext::new(&s.repo, s.policy.clone());
    let result = run_supervised_rehearsal(&s.task, &agents, &context).unwrap();
    assert_eq!(result.task.state, OrchestrationState::AwaitingApproval);
    assert_eq!(worker.borrow().calls, 1);
    assert_eq!(reviewer.borrow().calls, 1);
    assert!(!result.plan.worker.execution_enabled);
    assert!(!result.plan.reviewer.execution_enabled);
    // hermes_adapter::prepare canonicalizes with the Windows verbatim prefix; Python's does not.
    assert_eq!(
        result.plan.worker.workspace.trim_start_matches(r"\\?\"),
        s.task.workspace.to_string_lossy()
    );
    let approval = s
        .task
        .state_root
        .join(&s.task.brief.task_id)
        .join("approval-request.json");
    assert!(std::fs::read_to_string(approval).unwrap().contains("false"));
    assert_eq!(
        inspect_offline_task(&s.task).unwrap().state,
        OrchestrationState::AwaitingApproval
    );
    assert!(run_supervised_rehearsal(&s.task, &agents, &context).is_err());
    assert_eq!(worker.borrow().calls, 1);
    assert_eq!(reviewer.borrow().calls, 1);
}

#[test]
fn rehearsal_rejects_wrong_authorization_before_reservation() {
    let s = setup();
    let worker = RefCell::new(LocalDouble::new("worker", &s.task.workspace, true));
    let reviewer = RefCell::new(LocalDouble::new("reviewer", &s.task.workspace, false));
    let policy = SupervisedPolicy::new("wrong", &s.task.workspace);
    let err = run_supervised_rehearsal(
        &s.task,
        &OfflineAgents::new(&worker, &reviewer),
        &RehearsalContext::new(&s.repo, policy),
    )
    .unwrap_err();
    assert!(matches!(err, RehearsalError::Permission(_)));
    assert!(!s.task.state_root.join(&s.task.brief.task_id).exists());
}

#[test]
fn rehearsal_rejects_same_agent_before_reservation() {
    let s = setup();
    let worker = RefCell::new(LocalDouble::new("worker", &s.task.workspace, true));
    let err = run_supervised_rehearsal(
        &s.task,
        &OfflineAgents::new(&worker, &worker),
        &RehearsalContext::new(&s.repo, s.policy.clone()),
    )
    .unwrap_err();
    assert!(matches!(err, RehearsalError::Worktree(_)));
    assert!(!s.task.state_root.join(&s.task.brief.task_id).exists());
}

#[test]
fn worker_exception_stays_uncertain_without_retry() {
    let s = setup();
    let mut failing = LocalDouble::new("worker", &s.task.workspace, false);
    failing.fail = true;
    let worker = RefCell::new(failing);
    let reviewer = RefCell::new(LocalDouble::new("reviewer", &s.task.workspace, false));
    let agents = OfflineAgents::new(&worker, &reviewer);
    let context = RehearsalContext::new(&s.repo, s.policy.clone());
    let err = run_supervised_rehearsal(&s.task, &agents, &context).unwrap_err();
    assert!(err.to_string().contains("simulated worker failure"));
    assert_eq!(reviewer.borrow().calls, 0);
    assert_eq!(
        inspect_offline_task(&s.task).unwrap().state,
        OrchestrationState::DispatchUncertain
    );
    assert!(run_supervised_rehearsal(&s.task, &agents, &context).is_err());
    assert_eq!(worker.borrow().calls, 1);
}
