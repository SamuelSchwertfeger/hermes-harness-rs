#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline evidence-bound gateway reviewer handoff tests.

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use common::FakeLedger;
use hermes_harness::agents::supervised_agents::{
    SupervisedPlan, SupervisedPolicy, prepare_supervised_plan,
};
use hermes_harness::agents::supervised_review_handoff::{
    HandoffSources, ReviewerHandoffError, prepare_reviewer_handoff_once, verify_reviewer_handoff,
};
use hermes_harness::core::delegation::{Brief, DelegationError, Finding, OfflineAgent};
use hermes_harness::gateway::gateway_identity::Receipt;
use hermes_harness::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use hermes_harness::gateway::supervised_gateway_receipts::RoleAdmissions;
use hermes_harness::git::worktree_delegation::{
    TaskWorkspace, delegate_in_worktree, prepare_worktree,
};
use serde_json::{Value, json};

struct Worker<'a> {
    workspace: &'a Path,
}

impl OfflineAgent for Worker<'_> {
    fn run(&mut self, brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        assert!(prior.is_none());
        let target = self.workspace.join("src/safe.py");
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::write(target, "written\n")?;
        Ok(Finding {
            agent_id: brief.worker_id.clone(),
            text: "implemented".to_owned(),
        })
    }
}

struct Gateway {
    outcome: String,
    calls: Vec<String>,
}

impl Gateway {
    fn new(outcome: &str) -> Self {
        Self {
            outcome: outcome.to_owned(),
            calls: Vec::new(),
        }
    }
}

impl GatewayRpc for Gateway {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        self.calls.push(method.to_owned());
        Ok(json!({
            "admission_id": params["admission_id"],
            "ref": {"session_id": params["session_id"]},
            "status": "terminal",
            "outcome": self.outcome,
            "execution_generation": 1,
            "result": {
                "completed": self.outcome == "completed",
                "final_response": "worker finished",
            },
        }))
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(status.status.success(), "git {args:?} failed");
}

fn setup(tmp: &Path) -> (SupervisedPlan, TaskWorkspace, RoleAdmissions, FakeLedger) {
    let repo = tmp.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "offline@example.invalid"]);
    git(&repo, &["config", "user.name", "Offline"]);
    std::fs::write(repo.join("README.md"), "baseline\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "baseline"]);
    let root = tmp.join("state");
    std::fs::create_dir(&root).unwrap();
    let brief = Brief {
        task_id: "gateway-handoff".to_owned(),
        objective: "Offline task".to_owned(),
        owned_paths: vec!["src/safe.py".to_owned()],
        worker_id: "worker".to_owned(),
        reviewer_id: "reviewer".to_owned(),
    };
    let task = TaskWorkspace {
        state_root: root,
        workspace: tmp.join("worktree"),
        brief: brief.clone(),
    };
    prepare_worktree(&task, &repo).unwrap();
    delegate_in_worktree(
        &task,
        &mut Worker {
            workspace: &task.workspace,
        },
    )
    .unwrap();
    let policy = SupervisedPolicy::new(&brief.task_id, &task.workspace);
    let plan = prepare_supervised_plan(&brief, &policy).unwrap();
    let ids = RoleAdmissions {
        task_id: brief.task_id,
        worker_submission_id: "worker-sub".to_owned(),
        reviewer_submission_id: "review-sub".to_owned(),
    };
    let store = FakeLedger::default();
    store.prepare("worker-session", "worker-sub");
    store.record(
        "worker-sub",
        &Receipt {
            admission_id: "worker-admission".to_owned(),
            status: "queued".to_owned(),
            execution_generation: None,
        },
    );
    (plan, task, ids, store)
}

fn sources<'a>(
    ids: &'a RoleAdmissions,
    store: &'a FakeLedger,
    rpc: &'a mut Gateway,
) -> HandoffSources<'a, FakeLedger, Gateway> {
    HandoffSources { ids, store, rpc }
}

#[test]
fn successful_handoff_binds_audit_and_never_submits() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, task, ids, store) = setup(tmp.path());
    let mut gateway = Gateway::new("completed");
    let handoff =
        prepare_reviewer_handoff_once(&plan, &task, sources(&ids, &store, &mut gateway)).unwrap();
    assert!(handoff.reviewer_prompt.contains(&handoff.audit_sha256));
    assert!(handoff.reviewer_prompt.contains("worker-admission"));
    assert_eq!(gateway.calls, vec!["prompt.receipt"]);
    verify_reviewer_handoff(&task, &handoff).unwrap();
}

#[test]
fn changed_worker_file_invalidates_handoff() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, task, ids, store) = setup(tmp.path());
    let mut gateway = Gateway::new("completed");
    let handoff =
        prepare_reviewer_handoff_once(&plan, &task, sources(&ids, &store, &mut gateway)).unwrap();
    std::fs::write(task.workspace.join("src/safe.py"), "tampered\n").unwrap();
    let err = verify_reviewer_handoff(&task, &handoff).unwrap_err();
    assert!(err.to_string().contains("evidence changed"));
}

#[test]
fn failed_worker_never_prepares_reviewer() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, task, ids, store) = setup(tmp.path());
    let mut gateway = Gateway::new("failed");
    let err = prepare_reviewer_handoff_once(&plan, &task, sources(&ids, &store, &mut gateway))
        .unwrap_err();
    assert!(matches!(err, ReviewerHandoffError::Handoff(_)));
    assert!(err.to_string().contains("successful"));
    assert_eq!(gateway.calls, vec!["prompt.receipt"]);
}

#[test]
fn preexisting_reviewer_submission_blocks_without_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, task, ids, store) = setup(tmp.path());
    store.prepare("review-session", "review-sub");
    let mut gateway = Gateway::new("completed");
    let err = prepare_reviewer_handoff_once(&plan, &task, sources(&ids, &store, &mut gateway))
        .unwrap_err();
    assert!(err.to_string().contains("already submitted"));
    assert!(gateway.calls.is_empty());
}

#[test]
fn task_identity_mismatch_blocks_without_rpc() {
    let tmp = tempfile::tempdir().unwrap();
    let (plan, task, _ids, store) = setup(tmp.path());
    let wrong = RoleAdmissions {
        task_id: "wrong".to_owned(),
        worker_submission_id: "worker-sub".to_owned(),
        reviewer_submission_id: "review-sub".to_owned(),
    };
    let mut gateway = Gateway::new("completed");
    let err = prepare_reviewer_handoff_once(&plan, &task, sources(&wrong, &store, &mut gateway))
        .unwrap_err();
    assert!(err.to_string().contains("task identity"));
    assert!(gateway.calls.is_empty());
}
