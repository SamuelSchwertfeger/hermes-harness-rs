//! Durable lifecycle tests with an injected transport; no privileged subprocesses.
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

use std::cell::{Cell, RefCell};
use std::path::Path;

use hermes_harness::sandbox::live_sandbox_plan::{Role, SandboxInputs, build_sandbox_plan};
use hermes_harness::services::live_service_controller::{
    ServiceCommandTransport, ServiceControlError, ServiceController, TransportError,
};
use hermes_harness::services::live_service_journal::{ServiceJournal, ServiceState};
use tempfile::TempDir;

struct ScriptedTransport {
    calls: RefCell<Vec<Vec<String>>>,
    fail: Cell<bool>,
    inactive: Cell<bool>,
    active: Cell<bool>,
}

impl ScriptedTransport {
    fn new() -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            fail: Cell::new(false),
            inactive: Cell::new(true),
            active: Cell::new(true),
        }
    }
}

impl ServiceCommandTransport for ScriptedTransport {
    fn invoke(&self, argv: &[String]) -> Result<(), TransportError> {
        self.calls.borrow_mut().push(argv.to_vec());
        if self.fail.get() {
            return Err(std::io::Error::other("simulated uncertain transport outcome").into());
        }
        Ok(())
    }

    fn is_active(&self, _unit: &str) -> Result<bool, TransportError> {
        Ok(self.active.get())
    }

    fn is_inactive(&self, _unit: &str) -> Result<bool, TransportError> {
        Ok(self.inactive.get())
    }
}

struct Setup<'a> {
    controller: ServiceController<&'a ScriptedTransport>,
    transport: &'a ScriptedTransport,
    inputs: SandboxInputs,
}

fn make_inputs(tmp: &Path) -> SandboxInputs {
    let workspace = tmp.join("worktree");
    let evidence = tmp.join("evidence");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&evidence).unwrap();
    SandboxInputs::new(
        Role::Worker,
        "hermes-job-lifecycle",
        &workspace,
        &evidence,
        &Path::new("/usr/bin/env").canonicalize().unwrap(),
        &["probe"],
    )
}

fn setup<'a>(tmp: &Path, transport: &'a ScriptedTransport) -> Setup<'a> {
    let inputs = make_inputs(tmp);
    let controller = ServiceController::new(
        ServiceJournal::new(&tmp.join("journal.sqlite")).unwrap(),
        transport,
    );
    Setup {
        controller,
        transport,
        inputs,
    }
}

fn state(controller: &ServiceController<&ScriptedTransport>, unit: &str) -> ServiceState {
    controller.inspect(unit).unwrap().unwrap().state
}

fn is_journal(err: &ServiceControlError) -> bool {
    matches!(err, ServiceControlError::Journal(_))
}

#[test]
fn launch_and_stop_are_durable() {
    let tmp = TempDir::new().unwrap();
    let transport = ScriptedTransport::new();
    let Setup {
        controller, inputs, ..
    } = setup(tmp.path(), &transport);
    let plan = build_sandbox_plan(&inputs).unwrap();
    controller.launch_once(&plan).unwrap();
    assert_eq!(
        state(&controller, &plan.unit),
        ServiceState::RunningObserved
    );
    controller.stop_once(&plan).unwrap();
    assert_eq!(
        state(&controller, &plan.unit),
        ServiceState::StoppedObserved
    );
    assert_eq!(
        *transport.calls.borrow(),
        vec![plan.systemd_argv.clone(), plan.stop_argv.clone()]
    );
}

#[test]
fn duplicate_never_replays() {
    let tmp = TempDir::new().unwrap();
    let transport = ScriptedTransport::new();
    let Setup {
        controller, inputs, ..
    } = setup(tmp.path(), &transport);
    let plan = build_sandbox_plan(&inputs).unwrap();
    controller.launch_once(&plan).unwrap();
    let err = controller.launch_once(&plan).unwrap_err();
    assert!(is_journal(&err));
    assert!(err.to_string().contains("already reserved"));
    assert_eq!(*transport.calls.borrow(), vec![plan.systemd_argv.clone()]);
}

#[test]
fn uncertain_launch_fenced() {
    let tmp = TempDir::new().unwrap();
    let transport = ScriptedTransport::new();
    let Setup {
        controller, inputs, ..
    } = setup(tmp.path(), &transport);
    transport.fail.set(true);
    let plan = build_sandbox_plan(&inputs).unwrap();
    let err = controller.launch_once(&plan).unwrap_err();
    assert!(matches!(err, ServiceControlError::Transport(_)));
    assert!(
        err.to_string()
            .contains("simulated uncertain transport outcome")
    );
    assert_eq!(
        state(&controller, &plan.unit),
        ServiceState::LaunchUncertain
    );
    assert!(is_journal(&controller.launch_once(&plan).unwrap_err()));
    assert_eq!(transport.calls.borrow().len(), 1);
}

#[test]
fn unverified_stop_keeps_uncertainty() {
    let tmp = TempDir::new().unwrap();
    let transport = ScriptedTransport::new();
    let Setup {
        controller, inputs, ..
    } = setup(tmp.path(), &transport);
    let plan = build_sandbox_plan(&inputs).unwrap();
    controller.launch_once(&plan).unwrap();
    transport.inactive.set(false);
    let err = controller.stop_once(&plan).unwrap_err();
    assert!(matches!(err, ServiceControlError::Unsafe(_)));
    assert!(err.to_string().contains("unverified"));
    assert_eq!(state(&controller, &plan.unit), ServiceState::StopUncertain);
    assert!(matches!(
        controller.stop_once(&plan).unwrap_err(),
        ServiceControlError::Unsafe(_)
    ));
}

#[test]
fn crash_reopen_preserves_fence() {
    let tmp = TempDir::new().unwrap();
    let transport = ScriptedTransport::new();
    let Setup {
        controller, inputs, ..
    } = setup(tmp.path(), &transport);
    let plan = build_sandbox_plan(&inputs).unwrap();
    controller.launch_once(&plan).unwrap();
    let recovered = ServiceController::new(
        ServiceJournal::new(&controller.journal.path).unwrap(),
        &transport,
    );
    assert!(is_journal(&recovered.launch_once(&plan).unwrap_err()));
    assert_eq!(state(&recovered, &plan.unit), ServiceState::RunningObserved);
}

#[test]
fn mismatched_plan_refused() {
    let tmp = TempDir::new().unwrap();
    let transport = ScriptedTransport::new();
    let Setup {
        controller,
        inputs,
        transport,
    } = setup(tmp.path(), &transport);
    let plan = build_sandbox_plan(&inputs).unwrap();
    controller.launch_once(&plan).unwrap();
    let mut altered_inputs = inputs.clone();
    altered_inputs.role = Role::Reviewer;
    let altered = build_sandbox_plan(&altered_inputs).unwrap();
    let err = controller.stop_once(&altered).unwrap_err();
    assert!(matches!(err, ServiceControlError::Unsafe(_)));
    assert!(err.to_string().contains("mismatched"));
    assert_eq!(*transport.calls.borrow(), vec![plan.systemd_argv.clone()]);
}

#[test]
fn unverified_launch_fenced() {
    let tmp = TempDir::new().unwrap();
    let transport = ScriptedTransport::new();
    let Setup {
        controller, inputs, ..
    } = setup(tmp.path(), &transport);
    transport.active.set(false);
    let plan = build_sandbox_plan(&inputs).unwrap();
    let err = controller.launch_once(&plan).unwrap_err();
    assert!(matches!(err, ServiceControlError::Unsafe(_)));
    assert!(err.to_string().contains("unverified"));
    assert_eq!(
        state(&controller, &plan.unit),
        ServiceState::LaunchUncertain
    );
    assert!(is_journal(&controller.launch_once(&plan).unwrap_err()));
}
