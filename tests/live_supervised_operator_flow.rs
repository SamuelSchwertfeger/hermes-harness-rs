#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Provider-free checks for explicit operator role boundaries.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;

use hermes_harness::agents::supervised_agents::{
    SupervisedPlan, SupervisedPolicy, prepare_supervised_plan,
};
use hermes_harness::agents::supervised_review_handoff::ReviewerHandoff;
use hermes_harness::agents::supervised_reviewer_dispatch::DisabledRuntimeQualifier;
use hermes_harness::core::delegation::Brief;
use hermes_harness::gateway::admission_store::AdmissionStore;
use hermes_harness::gateway::gateway_identity::Submission;
use hermes_harness::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use hermes_harness::gateway::gateway_session_creation::{RpcFailure, SessionRpc};
use hermes_harness::gateway::scripted_supervised_gateway::ScriptedSupervisedGateway;
use hermes_harness::gateway::supervised_gateway_receipts::{ReceiptSources, RoleAdmissions};
use hermes_harness::git::worktree_delegation::TaskWorkspace;
use hermes_harness::sandbox::live_sandbox_plan::{Role, SandboxPlan};
use hermes_harness::supervised::live_execution_binding::ExecutionBinding;
use hermes_harness::supervised::live_execution_coordinator::LiveCoordinatorError;
use hermes_harness::supervised::live_supervised_finalization::FinalizationInputs;
use hermes_harness::supervised::live_supervised_operator_flow::{
    FlowCheckpoint, FlowCoordinator, OperatorFlowError, OperatorFlowInputs, SupervisedOperatorFlow,
    verify_sandbox_mounts,
};
use hermes_harness::supervised::live_supervised_recovery::{
    Phase, RecoveryCheckpoint, RecoveryIdentity, RecoveryLedger,
};
use hermes_harness::supervised::live_supervisor::LiveSupervisorInputs;
use serde_json::{Map, Value};
use tempfile::TempDir;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn base() -> Vec<String> {
    strings(&[
        "/usr/bin/systemd-run",
        "--unit=hermes-job-test",
        "/usr/bin/bwrap",
        "--unshare-net",
        "--unshare-pid",
        "--unshare-user",
        "--die-with-parent",
        "--ro-bind",
        "/usr",
        "/usr",
    ])
}

fn tail(items: &[&str]) -> Vec<String> {
    let mut out = base();
    out.extend(strings(items));
    out
}

fn worker_argv() -> Vec<String> {
    tail(&[
        "--bind",
        "/safe/worker",
        "/workspace",
        "--ro-bind",
        "/safe/evidence",
        "/evidence",
        "--chdir",
        "/workspace",
        "/usr/bin/python3",
    ])
}

fn reviewer_argv() -> Vec<String> {
    tail(&[
        "--ro-bind",
        "/safe/review",
        "/workspace",
        "--ro-bind",
        "/safe/evidence",
        "/evidence",
        "--chdir",
        "/workspace",
        "/usr/bin/python3",
    ])
}

fn plan_with(role: Role, unit: &str, argv: Vec<String>) -> SandboxPlan {
    SandboxPlan {
        role,
        unit: unit.to_owned(),
        systemd_argv: argv,
        stop_argv: Vec::new(),
    }
}

fn binding(role: &str, unit: &str, submission: &str, session: &str) -> ExecutionBinding {
    ExecutionBinding {
        task_id: "task".to_owned(),
        role: role.to_owned(),
        submission_id: submission.to_owned(),
        session_id: session.to_owned(),
        unit: unit.to_owned(),
    }
}

/// Exposes the scripted gateway through both RPC traits the supervisor needs.
struct Gateway(ScriptedSupervisedGateway);

impl GatewayRpc for Gateway {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        GatewayRpc::rpc(&mut self.0, method, params)
    }
}

impl SessionRpc for Gateway {
    fn rpc(&mut self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcFailure> {
        let converted: BTreeMap<String, Value> = params
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        GatewayRpc::rpc(&mut self.0, method, &converted).map_err(|error| Box::new(error) as _)
    }
}

struct Fixture {
    _tmp: TempDir,
    task: TaskWorkspace,
    plan: SupervisedPlan,
    ids: RoleAdmissions,
    worker: Submission,
    reviewer: Submission,
    handoff: ReviewerHandoff,
    store: AdmissionStore,
    recovery: RecoveryLedger,
    identity: RecoveryIdentity,
    output_dir: PathBuf,
    worker_binding: ExecutionBinding,
    reviewer_binding: ExecutionBinding,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let output_dir = tmp.path().join("out");
    std::fs::create_dir(&output_dir).unwrap();
    let brief = Brief {
        task_id: "task".to_owned(),
        objective: "Test operator flow".to_owned(),
        owned_paths: vec!["src/".to_owned()],
        worker_id: "worker".to_owned(),
        reviewer_id: "reviewer".to_owned(),
    };
    let policy = SupervisedPolicy::new(&brief.task_id, &workspace);
    Fixture {
        plan: prepare_supervised_plan(&brief, &policy).unwrap(),
        ids: RoleAdmissions {
            task_id: "task".to_owned(),
            worker_submission_id: "worker-sub".to_owned(),
            reviewer_submission_id: "reviewer-sub".to_owned(),
        },
        task: TaskWorkspace {
            state_root: tmp.path().join("state"),
            workspace,
            brief,
        },
        worker: Submission::new("worker-session", "worker-sub"),
        reviewer: Submission::new("reviewer-session", "reviewer-sub"),
        handoff: ReviewerHandoff {
            task_id: "task".to_owned(),
            reviewer_id: "reviewer".to_owned(),
            worker_submission_id: "worker-sub".to_owned(),
            worker_admission_id: "adm".to_owned(),
            audit_sha256: "a".repeat(64),
            reviewer_prompt: "prompt".to_owned(),
        },
        store: AdmissionStore::new(&tmp.path().join("admissions.sqlite")).unwrap(),
        recovery: RecoveryLedger::new(&tmp.path().join("recovery.db")).unwrap(),
        identity: RecoveryIdentity::new("task", "worker-sub", "reviewer-sub").unwrap(),
        output_dir,
        worker_binding: binding(
            "worker",
            "hermes-job-worker",
            "worker-sub",
            "worker-session",
        ),
        reviewer_binding: binding(
            "reviewer",
            "hermes-job-reviewer",
            "reviewer-sub",
            "reviewer-session",
        ),
        _tmp: tmp,
    }
}

/// Records every coordinator call; stands in for the patched Python mock.
#[derive(Default)]
struct FakeCoordinator {
    launches: RefCell<Vec<(ExecutionBinding, SandboxPlan)>>,
    worker_submits: RefCell<usize>,
    reviewer_submits: RefCell<usize>,
}

impl FlowCoordinator for FakeCoordinator {
    fn reserve_and_launch(
        &self,
        binding: &ExecutionBinding,
        plan: &SandboxPlan,
    ) -> Result<(), LiveCoordinatorError> {
        self.launches
            .borrow_mut()
            .push((binding.clone(), plan.clone()));
        Ok(())
    }

    fn submit_worker(
        &self,
        _binding: &ExecutionBinding,
        _inputs: &mut LiveSupervisorInputs<'_>,
    ) -> Result<(), LiveCoordinatorError> {
        *self.worker_submits.borrow_mut() += 1;
        Ok(())
    }

    fn submit_reviewer(
        &self,
        _binding: &ExecutionBinding,
        _inputs: &mut LiveSupervisorInputs<'_>,
    ) -> Result<(), LiveCoordinatorError> {
        *self.reviewer_submits.borrow_mut() += 1;
        Ok(())
    }
}

/// Stand-in for the patched `_checkpoint`.
struct FixedCheckpoint(Phase);

impl FlowCheckpoint for FixedCheckpoint {
    fn checkpoint<C: ?Sized, R: ?Sized>(
        &self,
        _inputs: &OperatorFlowInputs<'_, C, R>,
    ) -> Result<RecoveryCheckpoint, OperatorFlowError> {
        Ok(RecoveryCheckpoint {
            phase: self.0,
            evidence_sha256: String::new(),
            halted_reason: None,
        })
    }
}

fn flow_inputs<'a>(
    fx: &'a Fixture,
    supervisor_rpc: &'a mut Gateway,
    receipt_rpc: &'a mut ScriptedSupervisedGateway,
    coordinator: &'a FakeCoordinator,
) -> OperatorFlowInputs<'a, FakeCoordinator, ScriptedSupervisedGateway> {
    OperatorFlowInputs {
        coordinator,
        supervisor: LiveSupervisorInputs {
            task: &fx.task,
            plan: &fx.plan,
            ids: &fx.ids,
            worker_submission: &fx.worker,
            reviewer_submission: &fx.reviewer,
            store: &fx.store,
            rpc: supervisor_rpc,
            qualifier: &DisabledRuntimeQualifier,
        },
        finalization: FinalizationInputs {
            plan: &fx.plan,
            task: &fx.task,
            handoff: &fx.handoff,
            admissions: &fx.store,
            receipts: ReceiptSources {
                store: &fx.store,
                rpc: receipt_rpc,
            },
            recovery: &fx.recovery,
            identity: &fx.identity,
            output_dir: &fx.output_dir,
        },
        worker_binding: fx.worker_binding.clone(),
        reviewer_binding: fx.reviewer_binding.clone(),
        worker_plan: plan_with(Role::Worker, "hermes-job-worker", worker_argv()),
        reviewer_plan: plan_with(Role::Reviewer, "hermes-job-reviewer", reviewer_argv()),
    }
}

#[test]
fn reviewer_writable_bind_denied() {
    let mut argv = reviewer_argv();
    let at = argv.len() - 3;
    for (offset, item) in ["--bind", "/safe/extra", "/extra"].iter().enumerate() {
        argv.insert(at + offset, (*item).to_owned());
    }
    let err = verify_sandbox_mounts(
        &plan_with(Role::Worker, "w", worker_argv()),
        &plan_with(Role::Reviewer, "r", argv),
    )
    .unwrap_err();
    assert!(err.to_string().contains("read-only"));
}

#[test]
fn reviewer_without_network_isolation_denied() {
    let argv: Vec<String> = reviewer_argv()
        .into_iter()
        .filter(|x| x != "--unshare-net")
        .collect();
    let err = verify_sandbox_mounts(
        &plan_with(Role::Worker, "w", worker_argv()),
        &plan_with(Role::Reviewer, "r", argv),
    )
    .unwrap_err();
    assert!(err.to_string().contains("isolation"));
}

#[test]
fn no_implicit_authorization() {
    let fx = fixture();
    let (mut g1, mut g2) = (
        Gateway(ScriptedSupervisedGateway::default()),
        ScriptedSupervisedGateway::default(),
    );
    let coordinator = FakeCoordinator::default();
    let flow = SupervisedOperatorFlow::new(flow_inputs(&fx, &mut g1, &mut g2, &coordinator));
    assert!(
        flow.launch_worker(false)
            .unwrap_err()
            .to_string()
            .contains("authorization")
    );
    assert!(
        flow.launch_reviewer(false)
            .unwrap_err()
            .to_string()
            .contains("authorization")
    );
}

#[test]
fn wrong_phase_blocks_dispatch() {
    let fx = fixture();
    let (mut g1, mut g2) = (
        Gateway(ScriptedSupervisedGateway::default()),
        ScriptedSupervisedGateway::default(),
    );
    let coordinator = FakeCoordinator::default();
    let flow = SupervisedOperatorFlow::with_checkpoints(
        flow_inputs(&fx, &mut g1, &mut g2, &coordinator),
        FixedCheckpoint(Phase::Halted),
    );
    assert!(
        flow.launch_worker(true)
            .unwrap_err()
            .to_string()
            .contains("reconcile")
    );
    assert!(
        flow.launch_reviewer(true)
            .unwrap_err()
            .to_string()
            .contains("reconcile")
    );
}

#[test]
fn worker_launch_requires_explicit_authorization_before_inspection() {
    let fx = fixture();
    let (mut g1, mut g2) = (
        Gateway(ScriptedSupervisedGateway::default()),
        ScriptedSupervisedGateway::default(),
    );
    let coordinator = FakeCoordinator::default();
    let flow = SupervisedOperatorFlow::new(flow_inputs(&fx, &mut g1, &mut g2, &coordinator));
    assert!(
        flow.launch_worker(false)
            .unwrap_err()
            .to_string()
            .contains("authorization")
    );
    assert!(coordinator.launches.borrow().is_empty());
}

#[test]
fn reviewer_launch_does_not_submit_gateway_work() {
    let fx = fixture();
    let (mut g1, mut g2) = (
        Gateway(ScriptedSupervisedGateway::default()),
        ScriptedSupervisedGateway::default(),
    );
    let coordinator = FakeCoordinator::default();
    let flow = SupervisedOperatorFlow::with_checkpoints(
        flow_inputs(&fx, &mut g1, &mut g2, &coordinator),
        FixedCheckpoint(Phase::WorkerAudited),
    );
    flow.launch_reviewer(true).unwrap();
    let launches = coordinator.launches.borrow();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].0, flow.inputs.reviewer_binding);
    assert_eq!(launches[0].1, flow.inputs.reviewer_plan);
    assert_eq!(*coordinator.reviewer_submits.borrow(), 0);
}

#[test]
fn worker_launch_does_not_submit_gateway_work() {
    let fx = fixture();
    let (mut g1, mut g2) = (
        Gateway(ScriptedSupervisedGateway::default()),
        ScriptedSupervisedGateway::default(),
    );
    let coordinator = FakeCoordinator::default();
    let flow = SupervisedOperatorFlow::with_checkpoints(
        flow_inputs(&fx, &mut g1, &mut g2, &coordinator),
        FixedCheckpoint(Phase::Planned),
    );
    flow.launch_worker(true).unwrap();
    let launches = coordinator.launches.borrow();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].0, flow.inputs.worker_binding);
    assert_eq!(launches[0].1, flow.inputs.worker_plan);
    assert_eq!(*coordinator.worker_submits.borrow(), 0);
}
