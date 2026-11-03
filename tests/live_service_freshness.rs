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
//! Fresh service observation must gate live role dispatch, not journal history.

use std::cell::{Cell, RefCell};
use std::path::Path;

use hermes_harness::gateway::gateway_identity::Submission;
use hermes_harness::sandbox::live_sandbox_plan::{Role, SandboxInputs, build_sandbox_plan};
use hermes_harness::services::live_service_controller::{
    ServiceCommandTransport, ServiceController, TransportError,
};
use hermes_harness::services::live_service_journal::ServiceJournal;
use hermes_harness::supervised::live_execution_binding::{ExecutionBinding, ExecutionBindingStore};
use hermes_harness::supervised::live_execution_coordinator::{
    LiveCoordinatorError, LiveExecutionCoordinator, SupervisedDispatch,
};
use hermes_harness::supervised::live_supervisor::LiveSupervisorError;

/// Inert process observer with explicit failure injection.
struct FakeTransport {
    active: Cell<bool>,
    unavailable: Cell<bool>,
    queries: RefCell<Vec<String>>,
}

impl FakeTransport {
    fn new() -> Self {
        Self {
            active: Cell::new(true),
            unavailable: Cell::new(false),
            queries: RefCell::new(Vec::new()),
        }
    }
}

impl ServiceCommandTransport for FakeTransport {
    fn invoke(&self, _argv: &[String]) -> Result<(), TransportError> {
        Ok(())
    }

    fn is_active(&self, unit: &str) -> Result<bool, TransportError> {
        self.queries.borrow_mut().push(unit.to_owned());
        if self.unavailable.get() {
            return Err(std::io::Error::other("systemd unavailable").into());
        }
        Ok(self.active.get())
    }

    fn is_inactive(&self, _unit: &str) -> Result<bool, TransportError> {
        Ok(!self.active.get())
    }
}

/// Minimal gateway input shape: no RPC should ever be reached in these tests.
struct FakeInputs {
    task_id: String,
    worker: Submission,
    reviewer: Submission,
    submitted: Vec<&'static str>,
}

impl FakeInputs {
    fn for_binding(binding: &ExecutionBinding) -> Self {
        let submission = Submission::new(&binding.session_id, &binding.submission_id);
        Self {
            task_id: binding.task_id.clone(),
            worker: submission.clone(),
            reviewer: submission,
            submitted: Vec::new(),
        }
    }
}

impl SupervisedDispatch for FakeInputs {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn worker_submission(&self) -> &Submission {
        &self.worker
    }

    fn reviewer_submission(&self) -> &Submission {
        &self.reviewer
    }

    fn submit_worker_once(&mut self) -> Result<(), LiveSupervisorError> {
        self.submitted.push("worker");
        Ok(())
    }

    fn submit_reviewer_once(&mut self) -> Result<(), LiveSupervisorError> {
        self.submitted.push("reviewer");
        Ok(())
    }
}

fn plan_for(
    tmp: &Path,
    role: Role,
    unit: &str,
) -> hermes_harness::sandbox::live_sandbox_plan::SandboxPlan {
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

fn coordinator<'a>(
    tmp: &Path,
    transport: &'a FakeTransport,
) -> (
    LiveExecutionCoordinator<&'a FakeTransport>,
    ExecutionBinding,
) {
    std::fs::create_dir(tmp.join("worktree")).unwrap();
    std::fs::create_dir(tmp.join("evidence")).unwrap();
    let plan = plan_for(tmp, Role::Worker, "hermes-job-freshness");
    let services = ServiceController::new(
        ServiceJournal::new(&tmp.join("services.sqlite")).unwrap(),
        transport,
    );
    let bindings = ExecutionBindingStore::new(&tmp.join("bindings.sqlite")).unwrap();
    let binding = ExecutionBinding {
        task_id: "task".to_owned(),
        role: "worker".to_owned(),
        submission_id: "sub".to_owned(),
        session_id: "session".to_owned(),
        unit: plan.unit.clone(),
    };
    let coordinator = LiveExecutionCoordinator::new(bindings, services);
    coordinator.reserve_and_launch(&binding, &plan).unwrap();
    (coordinator, binding)
}

#[test]
fn status_requires_fresh_live_observation() {
    let tmp = tempfile::tempdir().unwrap();
    let transport = FakeTransport::new();
    let (coordinator, binding) = coordinator(tmp.path(), &transport);
    let before = coordinator.services.journal.get(&binding.unit).unwrap();
    assert!(coordinator.inspect_bound_service(&binding).unwrap());
    transport.active.set(false);
    assert!(!coordinator.inspect_bound_service(&binding).unwrap());
    assert_eq!(
        coordinator.services.journal.get(&binding.unit).unwrap(),
        before
    );
}

#[test]
fn stale_running_record_blocks_worker_submission() {
    let tmp = tempfile::tempdir().unwrap();
    let transport = FakeTransport::new();
    let (coordinator, binding) = coordinator(tmp.path(), &transport);
    transport.active.set(false);
    let mut inputs = FakeInputs::for_binding(&binding);
    let err = coordinator
        .submit_worker(&binding, &mut inputs)
        .unwrap_err();
    assert!(matches!(err, LiveCoordinatorError::Refused(_)));
    assert!(err.to_string().contains("no longer authoritatively active"));
    assert!(inputs.submitted.is_empty());
}

#[test]
fn inspection_uncertainty_blocks_gateway_submission() {
    let tmp = tempfile::tempdir().unwrap();
    let transport = FakeTransport::new();
    let (coordinator, binding) = coordinator(tmp.path(), &transport);
    transport.unavailable.set(true);
    let mut inputs = FakeInputs::for_binding(&binding);
    let err = coordinator
        .submit_worker(&binding, &mut inputs)
        .unwrap_err();
    assert!(matches!(err, LiveCoordinatorError::Transport(_)));
    assert!(err.to_string().contains("systemd unavailable"));
    assert!(inputs.submitted.is_empty());
}

#[test]
fn restart_still_checks_service() {
    let tmp = tempfile::tempdir().unwrap();
    let transport = FakeTransport::new();
    let (coordinator, binding) = coordinator(tmp.path(), &transport);
    let restarted = LiveExecutionCoordinator::new(
        ExecutionBindingStore::new(&coordinator.bindings.path).unwrap(),
        ServiceController::new(
            ServiceJournal::new(&coordinator.services.journal.path).unwrap(),
            &transport,
        ),
    );
    transport.active.set(false);
    assert!(!restarted.inspect_bound_service(&binding).unwrap());
    assert!(!transport.queries.borrow().is_empty());
}

#[test]
fn stale_reviewer_service_blocks_reviewer_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let transport = FakeTransport::new();
    let (coordinator, worker_binding) = coordinator(tmp.path(), &transport);
    let reviewer_binding = ExecutionBinding {
        task_id: worker_binding.task_id.clone(),
        role: "reviewer".to_owned(),
        submission_id: "review-sub".to_owned(),
        session_id: "review-session".to_owned(),
        unit: "hermes-job-review-freshness".to_owned(),
    };
    let reviewer_plan = plan_for(tmp.path(), Role::Reviewer, &reviewer_binding.unit);
    coordinator
        .reserve_and_launch(&reviewer_binding, &reviewer_plan)
        .unwrap();
    transport.active.set(false);
    let mut inputs = FakeInputs::for_binding(&reviewer_binding);
    let err = coordinator
        .submit_reviewer(&reviewer_binding, &mut inputs)
        .unwrap_err();
    assert!(err.to_string().contains("no longer authoritatively active"));
    assert!(inputs.submitted.is_empty());
}
