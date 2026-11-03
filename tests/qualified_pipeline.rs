#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
#![cfg(unix)]
//! Safety-focused pipeline unit tests (zero gateway or provider calls).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use hermes_harness::agents::supervised_agents::{
    SupervisedPlan, SupervisedPolicy, prepare_supervised_plan,
};
use hermes_harness::agents::supervised_reviewer_dispatch::DisabledRuntimeQualifier;
use hermes_harness::core::delegation::Brief;
use hermes_harness::gateway::admission_store::AdmissionStore;
use hermes_harness::gateway::gateway_identity::Submission;
use hermes_harness::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use hermes_harness::gateway::gateway_session_creation::{RpcFailure, SessionRpc};
use hermes_harness::gateway::scripted_supervised_gateway::ScriptedSupervisedGateway;
use hermes_harness::gateway::supervised_gateway_receipts::RoleAdmissions;
use hermes_harness::git::worktree_delegation::TaskWorkspace;
use hermes_harness::sandbox::live_sandbox_plan::{
    Role, SandboxInputs, SandboxPlan, build_sandbox_plan,
};
use hermes_harness::services::live_service_controller::{
    ServiceCommandTransport, ServiceController, TransportError,
};
use hermes_harness::services::live_service_journal::ServiceJournal;
use hermes_harness::supervised::live_execution_binding::{ExecutionBinding, ExecutionBindingStore};
use hermes_harness::supervised::live_execution_coordinator::LiveExecutionCoordinator;
use hermes_harness::supervised::live_supervised_recovery::{RecoveryIdentity, RecoveryLedger};
use hermes_harness::supervised::live_supervisor::LiveSupervisorInputs;
use hermes_harness::supervised::pipeline::{PipelineInputs, QualifiedPipeline};
use serde_json::{Map, Value};
use tempfile::TempDir;

/// Records every service command; nothing is executed.
#[derive(Default)]
struct FakeTransport {
    calls: RefCell<Vec<Vec<String>>>,
}

impl ServiceCommandTransport for FakeTransport {
    fn invoke(&self, argv: &[String]) -> Result<(), TransportError> {
        self.calls.borrow_mut().push(argv.to_vec());
        Ok(())
    }

    fn is_active(&self, _unit: &str) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn is_inactive(&self, _unit: &str) -> Result<bool, TransportError> {
        Ok(true)
    }
}

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
    store: AdmissionStore,
    worker_binding: ExecutionBinding,
    reviewer_binding: ExecutionBinding,
    worker_plan: SandboxPlan,
    reviewer_plan: SandboxPlan,
    recovery: RecoveryLedger,
    identity: RecoveryIdentity,
    coordinator: LiveExecutionCoordinator<&'static FakeTransport>,
}

fn sandbox(tmp: &Path, role: Role, unit: &str) -> SandboxPlan {
    build_sandbox_plan(&SandboxInputs::new(
        role,
        unit,
        &tmp.join("worktree"),
        &tmp.join("evidence"),
        &Path::new("/usr/bin/env").canonicalize().unwrap(),
        &["probe"],
    ))
    .unwrap()
}

fn binding(role: &str, unit: &str, submission_id: &str, session_id: &str) -> ExecutionBinding {
    ExecutionBinding {
        task_id: "task".to_owned(),
        role: role.to_owned(),
        submission_id: submission_id.to_owned(),
        session_id: session_id.to_owned(),
        unit: unit.to_owned(),
    }
}

fn fixture(transport: &'static FakeTransport) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("worktree");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(tmp.path().join("evidence")).unwrap();
    let brief = Brief {
        task_id: "task".to_owned(),
        objective: "Pipeline safety".to_owned(),
        owned_paths: vec!["src/".to_owned()],
        worker_id: "worker".to_owned(),
        reviewer_id: "reviewer".to_owned(),
    };
    let policy = SupervisedPolicy::new(&brief.task_id, &workspace);
    let services = ServiceController::new(
        ServiceJournal::new(&tmp.path().join("services.sqlite")).unwrap(),
        transport,
    );
    let bindings = ExecutionBindingStore::new(&tmp.path().join("bindings.sqlite")).unwrap();
    Fixture {
        plan: prepare_supervised_plan(&brief, &policy).unwrap(),
        ids: RoleAdmissions {
            task_id: "task".to_owned(),
            worker_submission_id: "w1".to_owned(),
            reviewer_submission_id: "r1".to_owned(),
        },
        task: TaskWorkspace {
            state_root: tmp.path().join("state"),
            workspace,
            brief,
        },
        worker: Submission::new("sw", "w1"),
        reviewer: Submission::new("sr", "r1"),
        store: AdmissionStore::new(&tmp.path().join("admissions.sqlite")).unwrap(),
        worker_binding: binding("worker", "hermes-job-w", "w1", "sw"),
        reviewer_binding: binding("reviewer", "hermes-job-r", "r1", "sr"),
        worker_plan: sandbox(tmp.path(), Role::Worker, "hermes-job-w"),
        reviewer_plan: sandbox(tmp.path(), Role::Reviewer, "hermes-job-r"),
        recovery: RecoveryLedger::new(&tmp.path().join("recovery.db")).unwrap(),
        identity: RecoveryIdentity::new("task", "w1", "r1").unwrap(),
        coordinator: LiveExecutionCoordinator::new(bindings, services),
        _tmp: tmp,
    }
}

fn pipeline<'a>(
    fx: &'a Fixture,
    gateway: &'a mut Gateway,
    reviewer_binding: &'a ExecutionBinding,
) -> QualifiedPipeline<'a, &'static FakeTransport> {
    QualifiedPipeline::new(PipelineInputs {
        coordinator: &fx.coordinator,
        supervisor: LiveSupervisorInputs {
            task: &fx.task,
            plan: &fx.plan,
            ids: &fx.ids,
            worker_submission: &fx.worker,
            reviewer_submission: &fx.reviewer,
            store: &fx.store,
            rpc: gateway,
            qualifier: &DisabledRuntimeQualifier,
        },
        worker_binding: &fx.worker_binding,
        reviewer_binding,
        worker_plan: &fx.worker_plan,
        reviewer_plan: &fx.reviewer_plan,
        recovery: &fx.recovery,
        identity: &fx.identity,
        output_dir: fx._tmp.path(),
    })
}

#[test]
fn worker_launch_requires_separate_authorization() {
    let transport: &'static FakeTransport = Box::leak(Box::default());
    let fx = fixture(transport);
    let mut gateway = Gateway(ScriptedSupervisedGateway::default());
    let err = pipeline(&fx, &mut gateway, &fx.reviewer_binding)
        .launch_worker(false)
        .unwrap_err();
    assert!(err.to_string().contains("authorization"));
    assert!(transport.calls.borrow().is_empty());
}

#[test]
fn role_identity_mismatch_blocks_even_when_authorized() {
    let transport: &'static FakeTransport = Box::leak(Box::default());
    let fx = fixture(transport);
    let mut gateway = Gateway(ScriptedSupervisedGateway::default());
    let clashing = binding("reviewer", "hermes-job-w", "r1", "sr");
    let err = pipeline(&fx, &mut gateway, &clashing)
        .launch_worker(true)
        .unwrap_err();
    assert!(err.to_string().contains("distinct"));
    assert!(transport.calls.borrow().is_empty());
}

#[test]
fn authorized_launch_is_single_explicit_action() {
    let transport: &'static FakeTransport = Box::leak(Box::default());
    let fx = fixture(transport);
    let mut gateway = Gateway(ScriptedSupervisedGateway::default());
    pipeline(&fx, &mut gateway, &fx.reviewer_binding)
        .launch_worker(true)
        .unwrap();
    assert_eq!(
        *transport.calls.borrow(),
        vec![fx.worker_plan.systemd_argv.clone()]
    );
    assert!(gateway.0.calls.is_empty());
}
