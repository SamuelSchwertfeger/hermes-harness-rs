#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Offline tests for bounded worktree delegation using disposable Git repos.
//!
//! Also covers the `offline_orchestrator` coordinator.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::core::delegation::{
    Brief, DelegationError, Finding, OfflineAgent, Phase, phase,
};
use hermes_harness::git::worktree_delegation::{
    TaskWorkspace, delegate_in_worktree, prepare_worktree, request_worktree_approval,
    review_worktree, verified_audit,
};
use hermes_harness::git::worktrees::resolve_strict;
use hermes_harness::supervised::offline_orchestrator::{
    OfflineAgents, OrchestrationState, inspect_offline_task, run_offline_task,
};

struct FakeAgent {
    identity: String,
    workspace: PathBuf,
    filename: Option<String>,
    calls: u32,
}

impl FakeAgent {
    fn new(identity: &str, workspace: &Path, filename: Option<&str>) -> Self {
        Self {
            identity: identity.to_owned(),
            workspace: workspace.to_path_buf(),
            filename: filename.map(str::to_owned),
            calls: 0,
        }
    }
}

impl OfflineAgent for FakeAgent {
    fn run(&mut self, _brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        self.calls += 1;
        if let Some(name) = &self.filename {
            let target = self.workspace.join(name);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, "offline fixture\n").unwrap();
        }
        Ok(Finding {
            agent_id: self.identity.clone(),
            text: if prior.is_some() {
                "reviewed"
            } else {
                "implemented"
            }
            .to_owned(),
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
    root: PathBuf,
    repo: PathBuf,
    workspace: PathBuf,
    brief: Brief,
}

impl Setup {
    fn task(&self) -> TaskWorkspace {
        TaskWorkspace {
            state_root: self.root.clone(),
            workspace: self.workspace.clone(),
            brief: self.brief.clone(),
        }
    }

    fn worker(&self, filename: Option<&str>) -> FakeAgent {
        FakeAgent::new("worker", &self.workspace, filename)
    }

    fn reviewer(&self) -> FakeAgent {
        FakeAgent::new("reviewer", &self.workspace, None)
    }

    fn prepared(&self) {
        prepare_worktree(&self.task(), &self.repo).unwrap();
    }

    fn delegated(&self) {
        self.prepared();
        delegate_in_worktree(&self.task(), &mut self.worker(Some("src/safe.py"))).unwrap();
    }
}

fn setup() -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let base = resolve_strict(tmp.path()).unwrap();
    let repo = base.join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "offline@example.invalid"],
        vec!["config", "user.name", "Offline"],
        vec!["config", "core.autocrlf", "false"],
    ] {
        git(&repo, &args);
    }
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "baseline"]);
    let root = base.join("evidence");
    std::fs::create_dir(&root).unwrap();
    Setup {
        workspace: base.join("worker-worktree"),
        brief: Brief {
            task_id: "ticket-one".to_owned(),
            objective: "Offline task".to_owned(),
            owned_paths: vec!["src/safe.py".to_owned()],
            worker_id: "worker".to_owned(),
            reviewer_id: "reviewer".to_owned(),
        },
        _tmp: tmp,
        root,
        repo,
    }
}

fn err_text<T, E: std::fmt::Display>(result: Result<T, E>) -> String {
    match result {
        Ok(_) => panic!("expected error"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn complete_audited_workflow() {
    let s = setup();
    s.delegated();
    let audit = verified_audit(&s.task()).unwrap();
    assert_eq!(audit.changed_paths, vec!["src/safe.py".to_owned()]);
    review_worktree(&s.task(), &mut s.reviewer()).unwrap();
    let approval = request_worktree_approval(&s.task()).unwrap();
    assert!(!approval.is_symlink());
    assert_eq!(phase(&s.root, &s.brief).unwrap(), Phase::AwaitingApproval);
}

#[test]
fn out_of_scope_write_blocks_review() {
    let s = setup();
    s.prepared();
    let text = err_text(delegate_in_worktree(
        &s.task(),
        &mut s.worker(Some("OTHER")),
    ));
    assert!(text.contains("outside ownership"), "{text}");
    let text = err_text(review_worktree(&s.task(), &mut s.reviewer()));
    assert!(text.contains("uncertain dispatch"), "{text}");
}

#[test]
fn worker_change_after_audit_blocks_review() {
    let s = setup();
    s.delegated();
    std::fs::write(s.workspace.join("other.txt"), "drift\n").unwrap();
    let _ = err_text(review_worktree(&s.task(), &mut s.reviewer()));
}

#[test]
fn review_without_audit_is_rejected() {
    let s = setup();
    s.prepared();
    let text = err_text(review_worktree(&s.task(), &mut s.reviewer()));
    assert!(text.contains("uncertain dispatch"), "{text}");
}

#[test]
fn cannot_reprepare_existing_worktree() {
    let s = setup();
    s.prepared();
    let _ = err_text(prepare_worktree(&s.task(), &s.repo));
}

#[test]
fn audit_tampering_blocks_review() {
    let s = setup();
    s.delegated();
    std::fs::write(
        s.root.join(&s.brief.task_id).join("worktree-audit.json"),
        "{}",
    )
    .unwrap();
    let text = err_text(review_worktree(&s.task(), &mut s.reviewer()));
    assert!(text.contains("uncertain dispatch"), "{text}");
}

#[test]
fn same_path_content_change_blocks_review() {
    let s = setup();
    s.delegated();
    std::fs::write(s.workspace.join("src/safe.py"), "altered after audit\n").unwrap();
    let text = err_text(review_worktree(&s.task(), &mut s.reviewer()));
    assert!(text.contains("evidence changed"), "{text}");
}

struct FailingAgent {
    message: &'static str,
    calls: u32,
}

impl OfflineAgent for FailingAgent {
    fn run(
        &mut self,
        _brief: &Brief,
        _prior: Option<&Finding>,
    ) -> Result<Finding, DelegationError> {
        self.calls += 1;
        Err(DelegationError::Agent(self.message.to_owned()))
    }
}

#[test]
fn coordinator_success_and_duplicate_refusal() {
    let s = setup();
    let task = s.task();
    let worker = RefCell::new(s.worker(Some("src/safe.py")));
    let reviewer = RefCell::new(s.reviewer());
    let agents = OfflineAgents::new(&worker, &reviewer);
    let result = run_offline_task(&task, &s.repo, &agents).unwrap();
    assert_eq!(result.state, OrchestrationState::AwaitingApproval);
    let approval = result.approval_request.unwrap();
    let bytes = std::fs::read(approval).unwrap();
    assert!(bytes.windows(16).any(|w| w == b"\"approved\":false"));
    assert_eq!((worker.borrow().calls, reviewer.borrow().calls), (1, 1));
    assert_eq!(
        inspect_offline_task(&task).unwrap().state,
        OrchestrationState::AwaitingApproval
    );
    assert!(run_offline_task(&task, &s.repo, &agents).is_err());
    assert_eq!((worker.borrow().calls, reviewer.borrow().calls), (1, 1));
}

#[test]
fn coordinator_worker_failure_is_not_retried() {
    let s = setup();
    let task = s.task();
    let worker = RefCell::new(FailingAgent {
        message: "offline fixture failure",
        calls: 0,
    });
    let reviewer = RefCell::new(s.reviewer());
    let agents = OfflineAgents::new(&worker, &reviewer);
    let err = run_offline_task(&task, &s.repo, &agents).unwrap_err();
    assert!(err.to_string().contains("offline fixture failure"));
    assert_eq!(
        inspect_offline_task(&task).unwrap().state,
        OrchestrationState::DispatchUncertain
    );
    assert!(run_offline_task(&task, &s.repo, &agents).is_err());
    assert_eq!((worker.borrow().calls, reviewer.borrow().calls), (1, 0));
}

#[test]
fn coordinator_rejects_same_agent_instance() {
    let s = setup();
    let agent = RefCell::new(s.worker(None));
    let err =
        run_offline_task(&s.task(), &s.repo, &OfflineAgents::new(&agent, &agent)).unwrap_err();
    assert!(err.to_string().contains("distinct identities"));
    assert!(!s.root.join(&s.brief.task_id).exists());
}

#[test]
fn coordinator_reviewer_failure_requires_manual_review() {
    let s = setup();
    let task = s.task();
    let worker = RefCell::new(s.worker(Some("src/safe.py")));
    let reviewer = RefCell::new(FailingAgent {
        message: "review interrupted",
        calls: 0,
    });
    let agents = OfflineAgents::new(&worker, &reviewer);
    let err = run_offline_task(&task, &s.repo, &agents).unwrap_err();
    assert!(err.to_string().contains("review interrupted"));
    assert_eq!(
        inspect_offline_task(&task).unwrap().state,
        OrchestrationState::ReviewUncertain
    );
    assert!(
        !s.root
            .join(&s.brief.task_id)
            .join("approval-request.json")
            .exists()
    );
    assert!(run_offline_task(&task, &s.repo, &agents).is_err());
    assert_eq!((worker.borrow().calls, reviewer.borrow().calls), (1, 1));
}
