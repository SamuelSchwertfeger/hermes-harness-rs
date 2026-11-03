#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! One-shot gateway staging without simulated Git worker success.

use std::collections::BTreeMap;
use std::path::Path;

use hermes_harness::agents::supervised_agents::{
    SupervisedPlan, SupervisedPolicy, prepare_supervised_plan,
};
use hermes_harness::agents::supervised_review_handoff::ReviewerHandoffError;
use hermes_harness::agents::supervised_reviewer_dispatch::{
    DisabledRuntimeQualifier, ReviewerDispatchBlockedError, RuntimeBinding, RuntimeQualifier,
};
use hermes_harness::core::delegation::{Brief, DelegationError};
use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_identity::{GatewayIdentityError, Submission};
use hermes_harness::gateway::gateway_live_admission::LiveAdmissionError;
use hermes_harness::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use hermes_harness::gateway::gateway_session_creation::{RpcFailure, SessionRpc};
use hermes_harness::gateway::scripted_supervised_gateway::ScriptedSupervisedGateway;
use hermes_harness::gateway::supervised_gateway_receipts::RoleAdmissions;
use hermes_harness::git::worktree_delegation::{TaskWorkspace, WorktreeDelegationError};
use hermes_harness::supervised::live_supervisor::{
    LiveSupervisorError, LiveSupervisorInputs, inspect_live_worker, submit_live_reviewer_once,
    submit_live_worker_once,
};
use serde_json::{Map, Value};

struct QualifiedTestRuntime;

impl RuntimeQualifier for QualifiedTestRuntime {
    fn require_qualified(
        &self,
        _binding: &RuntimeBinding,
    ) -> Result<(), ReviewerDispatchBlockedError> {
        Ok(())
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
    task: TaskWorkspace,
    plan: SupervisedPlan,
    ids: RoleAdmissions,
    worker: Submission,
    reviewer: Submission,
    store: AdmissionStore,
}

fn fixture(tmp: &Path) -> Fixture {
    let workspace = tmp.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let brief = Brief {
        task_id: "live-test".to_owned(),
        objective: "Test admission".to_owned(),
        owned_paths: vec!["src/".to_owned()],
        worker_id: "worker".to_owned(),
        reviewer_id: "reviewer".to_owned(),
    };
    let policy = SupervisedPolicy::new(&brief.task_id, &workspace);
    Fixture {
        plan: prepare_supervised_plan(&brief, &policy).unwrap(),
        ids: RoleAdmissions {
            task_id: brief.task_id.clone(),
            worker_submission_id: "worker-sub".to_owned(),
            reviewer_submission_id: "reviewer-sub".to_owned(),
        },
        task: TaskWorkspace {
            state_root: tmp.join("state"),
            workspace,
            brief,
        },
        worker: Submission::new("worker-session", "worker-sub"),
        reviewer: Submission::new("reviewer-session", "reviewer-sub"),
        store: AdmissionStore::new(&tmp.join("admissions.sqlite")).unwrap(),
    }
}

fn inputs<'a>(
    fx: &'a Fixture,
    gateway: &'a mut Gateway,
    qualifier: &'a dyn RuntimeQualifier,
) -> LiveSupervisorInputs<'a> {
    LiveSupervisorInputs {
        task: &fx.task,
        plan: &fx.plan,
        ids: &fx.ids,
        worker_submission: &fx.worker,
        reviewer_submission: &fx.reviewer,
        store: &fx.store,
        rpc: gateway,
        qualifier,
    }
}

fn submits(gateway: &Gateway) -> usize {
    gateway
        .0
        .calls
        .iter()
        .filter(|call| *call == "prompt.submit")
        .count()
}

#[test]
fn default_denies_before_submission() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture(tmp.path());
    let mut gateway = Gateway(ScriptedSupervisedGateway::default());
    let err = submit_live_worker_once(&mut inputs(&fx, &mut gateway, &DisabledRuntimeQualifier))
        .unwrap_err();
    assert!(matches!(
        err,
        LiveSupervisorError::Dispatch(ReviewerDispatchBlockedError::Blocked(_))
    ));
    assert!(gateway.0.calls.is_empty());
    assert!(fx.store.get("worker-sub").unwrap().is_none());
}

#[test]
fn one_shot_and_receipt_with_external_test_qualifier() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture(tmp.path());
    let mut gateway = Gateway(ScriptedSupervisedGateway::default());
    let mut live = inputs(&fx, &mut gateway, &QualifiedTestRuntime);
    submit_live_worker_once(&mut live).unwrap();
    let evidence = inspect_live_worker(&mut live).unwrap();
    assert_eq!(evidence.outcome, "completed");
    let err = submit_live_worker_once(&mut live).unwrap_err();
    assert!(matches!(err, LiveSupervisorError::Blocked(_)));
    assert!(err.to_string().contains("already recorded"));
    let err = submit_live_reviewer_once(&mut live).unwrap_err();
    assert!(matches!(
        err,
        LiveSupervisorError::Handoff(ReviewerHandoffError::Worktree(
            WorktreeDelegationError::Delegation(DelegationError::Message(_))
        ))
    ));
    assert_eq!(submits(&gateway), 1);
}

#[test]
fn unknown_outcome_no_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture(tmp.path());
    let mut gateway = Gateway(ScriptedSupervisedGateway {
        fail_submission_role: "worker".to_owned(),
        ..ScriptedSupervisedGateway::default()
    });
    let mut live = inputs(&fx, &mut gateway, &QualifiedTestRuntime);
    let err = submit_live_worker_once(&mut live).unwrap_err();
    assert!(matches!(
        err,
        LiveSupervisorError::Admission(LiveAdmissionError::Identity(
            GatewayIdentityError::OutcomeUnknown { .. }
        ))
    ));
    let record = fx.store.get("worker-sub").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    let err = submit_live_worker_once(&mut live).unwrap_err();
    assert!(matches!(err, LiveSupervisorError::Blocked(_)));
    assert_eq!(submits(&gateway), 1);
}
