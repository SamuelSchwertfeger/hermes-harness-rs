//! Stop reconciliation never calls systemd-stop twice or relaunches a service.
#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]

use std::cell::{Cell, RefCell};
use std::path::Path;

use hermes_harness::services::live_service_controller::{
    ServiceCommandTransport, ServiceControlError, ServiceController, TransportError,
};
use hermes_harness::services::live_service_journal::{ServiceJournal, ServiceState};
use tempfile::TempDir;

struct ObservationTransport {
    inactive: Cell<bool>,
    calls: RefCell<Vec<String>>,
    fail: Cell<bool>,
}

impl ServiceCommandTransport for ObservationTransport {
    fn invoke(&self, _argv: &[String]) -> Result<(), TransportError> {
        panic!("Unexpected service side effect during recovery");
    }

    fn is_active(&self, _unit: &str) -> Result<bool, TransportError> {
        panic!("Recovery must not request an active observation");
    }

    fn is_inactive(&self, unit: &str) -> Result<bool, TransportError> {
        self.calls.borrow_mut().push(unit.to_owned());
        if self.fail.get() {
            return Err(std::io::Error::other("simulated systemd inspection failure").into());
        }
        Ok(self.inactive.get())
    }
}

const fn transport() -> ObservationTransport {
    ObservationTransport {
        inactive: Cell::new(false),
        calls: RefCell::new(Vec::new()),
        fail: Cell::new(false),
    }
}

fn controller<'a>(
    tmp: &Path,
    state: ServiceState,
    transport: &'a ObservationTransport,
) -> ServiceController<&'a ObservationTransport> {
    let journal = ServiceJournal::new(&tmp.join("services.sqlite")).unwrap();
    journal.reserve("hermes-job-stop", "worker", "{}").unwrap();
    if state != ServiceState::LaunchUncertain {
        journal
            .transition(
                "hermes-job-stop",
                ServiceState::LaunchUncertain,
                ServiceState::RunningObserved,
            )
            .unwrap();
    }
    if state == ServiceState::StopUncertain {
        journal
            .transition(
                "hermes-job-stop",
                ServiceState::RunningObserved,
                ServiceState::StopUncertain,
            )
            .unwrap();
    }
    ServiceController::new(journal, transport)
}

fn current(controller: &ServiceController<&ObservationTransport>) -> ServiceState {
    controller
        .inspect("hermes-job-stop")
        .unwrap()
        .unwrap()
        .state
}

#[test]
fn reconciles_uncertain_stop_only_after_inactive_observation() {
    let tmp = TempDir::new().unwrap();
    let transport = transport();
    let controller = controller(tmp.path(), ServiceState::StopUncertain, &transport);
    transport.inactive.set(true);
    let result = controller.reconcile_stopped("hermes-job-stop").unwrap();
    assert_eq!(result.state, ServiceState::StoppedObserved);
    assert_eq!(*transport.calls.borrow(), vec!["hermes-job-stop"]);
    assert_eq!(
        ServiceJournal::new(&controller.journal.path)
            .unwrap()
            .get("hermes-job-stop")
            .unwrap(),
        Some(result)
    );
}

#[test]
fn unknown_stop_remains_durable() {
    let tmp = TempDir::new().unwrap();
    let transport = transport();
    let controller = controller(tmp.path(), ServiceState::StopUncertain, &transport);
    let err = controller.reconcile_stopped("hermes-job-stop").unwrap_err();
    assert!(err.to_string().contains("still unknown"));
    assert_eq!(current(&controller), ServiceState::StopUncertain);
    assert_eq!(*transport.calls.borrow(), vec!["hermes-job-stop"]);
}

#[test]
fn transport_failure_does_not_clear_uncertainty() {
    let tmp = TempDir::new().unwrap();
    let transport = transport();
    let controller = controller(tmp.path(), ServiceState::StopUncertain, &transport);
    transport.fail.set(true);
    let err = controller.reconcile_stopped("hermes-job-stop").unwrap_err();
    assert!(matches!(err, ServiceControlError::Transport(_)));
    assert!(
        err.to_string()
            .contains("simulated systemd inspection failure")
    );
    assert_eq!(current(&controller), ServiceState::StopUncertain);
}

#[test]
fn launch_uncertain_cannot_become_running_via_stop_recovery() {
    let tmp = TempDir::new().unwrap();
    let transport = transport();
    let controller = controller(tmp.path(), ServiceState::LaunchUncertain, &transport);
    transport.inactive.set(true);
    let err = controller.reconcile_stopped("hermes-job-stop").unwrap_err();
    assert!(err.to_string().contains("previously uncertain stop"));
    assert!(transport.calls.borrow().is_empty());
}

#[test]
fn running_service_cannot_be_stopped_implicitly() {
    let tmp = TempDir::new().unwrap();
    let transport = transport();
    let controller = controller(tmp.path(), ServiceState::RunningObserved, &transport);
    transport.inactive.set(true);
    let err = controller.reconcile_stopped("hermes-job-stop").unwrap_err();
    assert!(err.to_string().contains("previously uncertain stop"));
    assert!(transport.calls.borrow().is_empty());
}

#[test]
fn second_reconciliation_denied_without_probing_again() {
    let tmp = TempDir::new().unwrap();
    let transport = transport();
    let controller = controller(tmp.path(), ServiceState::StopUncertain, &transport);
    transport.inactive.set(true);
    controller.reconcile_stopped("hermes-job-stop").unwrap();
    let err = controller.reconcile_stopped("hermes-job-stop").unwrap_err();
    assert!(err.to_string().contains("previously uncertain stop"));
    assert_eq!(*transport.calls.borrow(), vec!["hermes-job-stop"]);
}
