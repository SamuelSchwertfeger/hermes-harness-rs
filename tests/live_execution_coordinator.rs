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
//! Controller and gateway dispatch are fenced by exact durable identities.

use std::cell::RefCell;
use std::path::Path;

use hermes_harness::sandbox::live_sandbox_plan::{Role, SandboxInputs, build_sandbox_plan};
use hermes_harness::services::live_service_controller::{
    ServiceCommandTransport, ServiceController, TransportError,
};
use hermes_harness::services::live_service_journal::ServiceJournal;
use hermes_harness::supervised::live_execution_binding::{ExecutionBinding, ExecutionBindingStore};
use hermes_harness::supervised::live_execution_coordinator::{
    LiveCoordinatorError, LiveExecutionCoordinator,
};

/// Exercise durable launch without invoking privileged host operations.
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

fn plan_for(tmp: &Path, unit: &str) -> hermes_harness::sandbox::live_sandbox_plan::SandboxPlan {
    let workspace = tmp.join("worktree");
    let evidence = tmp.join("evidence");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&evidence).unwrap();
    build_sandbox_plan(&SandboxInputs::new(
        Role::Worker,
        unit,
        &workspace,
        &evidence,
        &Path::new("/usr/bin/env").canonicalize().unwrap(),
        &["probe"],
    ))
    .unwrap()
}

fn binding(role: &str, unit: &str) -> ExecutionBinding {
    ExecutionBinding {
        task_id: "task".to_owned(),
        role: role.to_owned(),
        submission_id: "sub".to_owned(),
        session_id: "session".to_owned(),
        unit: unit.to_owned(),
    }
}

#[test]
fn launch_bind_then_refuse_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = plan_for(tmp.path(), "hermes-job-coordinator");
    let transport = FakeTransport::default();
    let services = ServiceController::new(
        ServiceJournal::new(&tmp.path().join("services.sqlite")).unwrap(),
        &transport,
    );
    let bindings = ExecutionBindingStore::new(&tmp.path().join("bindings.sqlite")).unwrap();
    let coordinator = LiveExecutionCoordinator::new(bindings, services);
    let binding = binding("worker", &plan.unit);
    coordinator.reserve_and_launch(&binding, &plan).unwrap();
    assert_eq!(*transport.calls.borrow(), vec![plan.systemd_argv.clone()]);
    coordinator
        .bindings
        .require_running(&binding, &coordinator.services.journal)
        .unwrap();
    let restarted = LiveExecutionCoordinator::new(
        ExecutionBindingStore::new(&coordinator.bindings.path).unwrap(),
        ServiceController::new(coordinator.services.journal.clone(), &transport),
    );
    let err = restarted.reserve_and_launch(&binding, &plan).unwrap_err();
    assert!(matches!(err, LiveCoordinatorError::Binding(_)));
    assert!(
        err.to_string()
            .contains("Service unit already used or uncertain")
    );
    assert_eq!(*transport.calls.borrow(), vec![plan.systemd_argv.clone()]);
}

#[test]
fn mismatched_plan_denied_before_side_effect() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = plan_for(tmp.path(), "hermes-job-mismatch");
    let transport = FakeTransport::default();
    let bindings = ExecutionBindingStore::new(&tmp.path().join("bindings.sqlite")).unwrap();
    let coordinator = LiveExecutionCoordinator::new(
        bindings,
        ServiceController::new(
            ServiceJournal::new(&tmp.path().join("services.sqlite")).unwrap(),
            &transport,
        ),
    );
    let wrong = binding("reviewer", &plan.unit);
    let err = coordinator.reserve_and_launch(&wrong, &plan).unwrap_err();
    assert!(matches!(err, LiveCoordinatorError::Refused(_)));
    assert!(err.to_string().contains("Plan does not match"));
    assert!(
        coordinator
            .bindings
            .get("task", "reviewer")
            .unwrap()
            .is_none()
    );
    assert!(transport.calls.borrow().is_empty());
}
