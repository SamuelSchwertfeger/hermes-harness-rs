#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Disposable Git end-to-end supervised scripted gateway qualification.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::agents::supervised_agents::SupervisedPolicy;
use hermes_harness::agents::supervised_execution::{
    ExecutionBlockedError, ExecutionRpc, ScriptedExecution, execute_scripted_once, inspect_scripted,
};
use hermes_harness::agents::supervised_reviewer_dispatch::{
    DisabledRuntimeQualifier, ReviewerDispatchBlockedError, RuntimeBinding, RuntimeQualifier,
};
use hermes_harness::agents::supervised_status::inspect;
use hermes_harness::core::delegation::{Brief, DelegationError, Finding, OfflineAgent};
use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_identity::{GatewayIdentityError, Submission};
use hermes_harness::gateway::gateway_live_admission::LiveAdmissionError;
use hermes_harness::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use hermes_harness::gateway::scripted_supervised_gateway::ScriptedSupervisedGateway;
use hermes_harness::git::worktree_delegation::TaskWorkspace;
use hermes_harness::util::resolve_strict;
use serde_json::Value;

const EXPECTED_ROLE_SUBMISSIONS: usize = 2;

#[derive(Default)]
struct QualifiedScripted {
    roles: RefCell<Vec<String>>,
}

impl RuntimeQualifier for QualifiedScripted {
    fn require_qualified(
        &self,
        binding: &RuntimeBinding,
    ) -> Result<(), ReviewerDispatchBlockedError> {
        self.roles.borrow_mut().push(binding.reviewer_id.clone());
        Ok(())
    }
}

struct Worker {
    workspace: PathBuf,
}

impl OfflineAgent for Worker {
    fn run(&mut self, brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        assert!(prior.is_none());
        let target = self.workspace.join("src").join("changed.txt");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, "worker output\n").unwrap();
        Ok(Finding {
            agent_id: brief.worker_id.clone(),
            text: "worker implementation".to_owned(),
        })
    }
}

struct Reviewer;

impl OfflineAgent for Reviewer {
    fn run(&mut self, brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        let prior = prior.unwrap();
        assert_eq!(prior.agent_id, brief.worker_id);
        Ok(Finding {
            agent_id: brief.reviewer_id.clone(),
            text: "independent review: requires human approval".to_owned(),
        })
    }
}

struct NotScripted;

impl GatewayRpc for NotScripted {
    fn rpc(
        &mut self,
        _method: &str,
        _params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        Err(GatewayRpcError::Value("must not be called".to_owned()))
    }
}

impl ExecutionRpc for NotScripted {
    fn is_builtin_scripted(&self) -> bool {
        false
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

struct Fixture {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    gateway: RefCell<ScriptedSupervisedGateway>,
    qualifier: QualifiedScripted,
    worker: RefCell<Worker>,
    reviewer: RefCell<Reviewer>,
    task: TaskWorkspace,
    repo: PathBuf,
}

impl Fixture {
    fn new(gateway: ScriptedSupervisedGateway) -> Self {
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
        std::fs::write(repo.join("README.md"), "baseline\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "baseline"]);
        let state = base.join("state");
        std::fs::create_dir(&state).unwrap();
        let workspace = base.join("workspace");
        let brief = Brief {
            task_id: "integrated".to_owned(),
            objective: "Offline qualification".to_owned(),
            owned_paths: vec!["src/changed.txt".to_owned()],
            worker_id: "worker".to_owned(),
            reviewer_id: "reviewer".to_owned(),
        };
        Self {
            _tmp: tmp,
            base,
            gateway: RefCell::new(gateway),
            qualifier: QualifiedScripted::default(),
            worker: RefCell::new(Worker {
                workspace: workspace.clone(),
            }),
            reviewer: RefCell::new(Reviewer),
            task: TaskWorkspace {
                state_root: state,
                workspace,
                brief,
            },
            repo,
        }
    }

    fn ledger(&self) -> PathBuf {
        self.base.join("admissions.sqlite")
    }

    fn config(&self) -> ScriptedExecution<'_> {
        ScriptedExecution {
            policy: SupervisedPolicy::new(&self.task.brief.task_id, &self.task.workspace),
            task: self.task.clone(),
            source_repo: self.repo.clone(),
            worker_submission: Submission::new("worker-session", "worker-sub"),
            reviewer_submission: Submission::new("review-session", "review-sub"),
            ledger_path: self.ledger(),
            rpc: &self.gateway,
            qualifier: &self.qualifier,
            worker: &self.worker,
            reviewer: &self.reviewer,
        }
    }

    fn submits(&self) -> usize {
        self.gateway
            .borrow()
            .calls
            .iter()
            .filter(|call| call.as_str() == "prompt.submit")
            .count()
    }
}

#[test]
fn end_to_end_and_duplicate_fence() {
    let f = Fixture::new(ScriptedSupervisedGateway::default());
    let config = f.config();
    let result = execute_scripted_once(&config).unwrap();
    assert_ne!(result.worker_admission_id, result.reviewer_admission_id);
    let approval: Value =
        serde_json::from_str(&std::fs::read_to_string(&result.approval_request).unwrap()).unwrap();
    assert_eq!(approval["approved"], false);
    assert_eq!(f.submits(), EXPECTED_ROLE_SUBMISSIONS);
    assert_eq!(*f.qualifier.roles.borrow(), ["worker", "reviewer"]);
    assert_eq!(
        inspect_scripted(&f.task, &f.ledger()).unwrap().phase,
        "awaiting_approval"
    );
    let err = execute_scripted_once(&config).unwrap_err();
    assert!(matches!(err, ExecutionBlockedError::Blocked(_)));
    assert!(err.to_string().contains("Existing ledger"));
    assert_eq!(f.submits(), EXPECTED_ROLE_SUBMISSIONS);
}

#[test]
fn worker_failure_blocks_reviewer() {
    let f = Fixture::new(ScriptedSupervisedGateway {
        worker_outcome: "failed".to_owned(),
        ..ScriptedSupervisedGateway::default()
    });
    let err = execute_scripted_once(&f.config()).unwrap_err();
    assert!(matches!(err, ExecutionBlockedError::Blocked(_)));
    assert!(err.to_string().contains("Worker receipt"));
    assert_eq!(f.submits(), 1);
    assert!(!f.task.workspace.join("src").join("changed.txt").exists());
}

#[test]
fn uncertain_worker_submission_fenced() {
    let f = Fixture::new(ScriptedSupervisedGateway {
        fail_submission_role: "worker".to_owned(),
        ..ScriptedSupervisedGateway::default()
    });
    let config = f.config();
    let err = execute_scripted_once(&config).unwrap_err();
    assert!(matches!(
        err,
        ExecutionBlockedError::Admission(LiveAdmissionError::Identity(
            GatewayIdentityError::OutcomeUnknown { .. }
        ))
    ));
    let record = AdmissionStore::new(&f.ledger())
        .unwrap()
        .get("worker-sub")
        .unwrap()
        .unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    let again = execute_scripted_once(&config).unwrap_err();
    assert!(matches!(again, ExecutionBlockedError::Blocked(_)));
    assert_eq!(f.submits(), 1);
}

#[test]
fn denied_qualifier_never_submits() {
    let f = Fixture::new(ScriptedSupervisedGateway::default());
    let config = ScriptedExecution {
        qualifier: &DisabledRuntimeQualifier,
        ..f.config()
    };
    let err = execute_scripted_once(&config).unwrap_err();
    assert!(err.to_string().contains("disabled"));
    assert!(f.gateway.borrow().calls.is_empty());
}

#[test]
fn identity_refusal() {
    let worker_session = Submission::new("review-session", "another");
    let reviewer_id = Submission::new("review-session", "worker-sub");
    for (field, value) in [
        ("worker_submission", worker_session),
        ("reviewer_submission", reviewer_id),
    ] {
        let f = Fixture::new(ScriptedSupervisedGateway::default());
        let config = match field {
            "worker_submission" => ScriptedExecution {
                worker_submission: value,
                ..f.config()
            },
            _ => ScriptedExecution {
                reviewer_submission: value,
                ..f.config()
            },
        };
        let err = execute_scripted_once(&config).unwrap_err();
        assert!(matches!(err, ExecutionBlockedError::Blocked(_)), "{field}");
    }
}

#[test]
fn reviewer_failure_never_requests_approval() {
    let f = Fixture::new(ScriptedSupervisedGateway {
        reviewer_outcome: "failed".to_owned(),
        ..ScriptedSupervisedGateway::default()
    });
    let err = execute_scripted_once(&f.config()).unwrap_err();
    assert!(err.to_string().contains("Reviewer receipt"));
    assert_eq!(f.submits(), EXPECTED_ROLE_SUBMISSIONS);
    assert!(
        !f.task
            .state_root
            .join("integrated")
            .join("approval-request.json")
            .exists()
    );
    assert_ne!(
        inspect_scripted(&f.task, &f.ledger()).unwrap().phase,
        "awaiting_approval"
    );
}

#[test]
fn non_scripted_gateway_fails_before_worktree() {
    let f = Fixture::new(ScriptedSupervisedGateway::default());
    let other = RefCell::new(NotScripted);
    let config = ScriptedExecution {
        rpc: &other,
        ..f.config()
    };
    let err = execute_scripted_once(&config).unwrap_err();
    assert!(err.to_string().contains("network-free"));
    assert!(!f.task.workspace.exists());
}

#[test]
fn readonly_status_does_not_create_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("missing");
    let result = inspect(&root, "scripted-demo").unwrap();
    assert!(result.admissions.is_empty());
    assert_eq!(result.approved, None);
    assert!(!root.exists());
}

#[test]
fn readonly_status_after_success() {
    let f = Fixture::new(ScriptedSupervisedGateway::default());
    execute_scripted_once(&f.config()).unwrap();
    let before = std::fs::metadata(f.ledger()).unwrap().modified().unwrap();
    let status = inspect(&f.base, "integrated").unwrap();
    assert_eq!(status.approved, Some(false));
    assert_eq!(status.admissions.len(), EXPECTED_ROLE_SUBMISSIONS);
    assert_eq!(
        std::fs::metadata(f.ledger()).unwrap().modified().unwrap(),
        before
    );
}
