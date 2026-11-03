#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Recovery inspection is read-only, including on failed status probes.

use std::cell::{Cell, RefCell};

use hermes_harness::operator::live_operator_reconciliation::{
    RecoveryInspectionRequest, RecoveryReader, ServiceInspector, inspect_operator_recovery,
};
use hermes_harness::supervised::live_execution_binding::ExecutionBinding;
use hermes_harness::supervised::live_execution_coordinator::LiveCoordinatorError;
use hermes_harness::supervised::live_supervised_recovery::{
    Phase, RecoveryCheckpoint, RecoveryError, RecoveryIdentity,
};

/// Scripted coordinator; only the read-only probe exists, so no dispatch can happen.
struct FakeCoordinator {
    results: RefCell<Vec<Result<bool, LiveCoordinatorError>>>,
}

impl ServiceInspector for FakeCoordinator {
    fn inspect_bound_service(
        &self,
        _binding: &ExecutionBinding,
    ) -> Result<bool, LiveCoordinatorError> {
        self.results.borrow_mut().remove(0)
    }
}

struct FakeLedger {
    checkpoint: RecoveryCheckpoint,
    reads: Cell<usize>,
}

impl RecoveryReader for FakeLedger {
    fn read(&self, _identity: &RecoveryIdentity) -> Result<RecoveryCheckpoint, RecoveryError> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.checkpoint.clone())
    }
}

fn ledger(phase: Phase, evidence: &str, halted: Option<&str>) -> FakeLedger {
    FakeLedger {
        checkpoint: RecoveryCheckpoint {
            phase,
            evidence_sha256: evidence.to_owned(),
            halted_reason: halted.map(str::to_owned),
        },
        reads: Cell::new(0),
    }
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

fn identity() -> RecoveryIdentity {
    RecoveryIdentity::new("task", "worker", "reviewer").unwrap()
}

#[test]
fn reconciliation_reads_without_dispatch() {
    let coordinator = FakeCoordinator {
        results: RefCell::new(vec![Ok(true), Ok(false)]),
    };
    let ledger = ledger(Phase::PendingHuman, "digest", None);
    let worker = binding("worker", "hermes-job-worker");
    let reviewer = binding("reviewer", "hermes-job-reviewer");
    let id = identity();
    let report = inspect_operator_recovery(
        &coordinator,
        &ledger,
        &RecoveryInspectionRequest {
            identity: &id,
            bindings: [&worker, &reviewer],
        },
    )
    .unwrap();
    assert_eq!(report.phase, Phase::PendingHuman);
    assert_eq!(report.services[0].active, Some(true));
    assert_eq!(report.services[1].active, Some(false));
    assert!(report.guidance.contains("pending"));
    assert_eq!(ledger.reads.get(), 1);
}

#[test]
fn uncertain_service_blocks_recovery_guidance() {
    let coordinator = FakeCoordinator {
        results: RefCell::new(vec![
            Err(LiveCoordinatorError::Refused("secret details".to_owned())),
            Err(LiveCoordinatorError::Refused("secret details".to_owned())),
        ]),
    };
    let ledger = ledger(Phase::WorkerAdmitted, "digest", None);
    let binding = binding("worker", "hermes-job-worker");
    let id = identity();
    let report = inspect_operator_recovery(
        &coordinator,
        &ledger,
        &RecoveryInspectionRequest {
            identity: &id,
            bindings: [&binding, &binding],
        },
    )
    .unwrap();
    assert!(report.requires_investigation);
    assert!(
        report
            .services
            .iter()
            .all(|item| item.error.as_deref() == Some("status unverified"))
    );
    assert!(!format!("{report:?}").contains("secret details"));
    assert!(report.guidance.contains("do not replay"));
}

#[test]
fn halted_checkpoint_never_recommends_resume() {
    let coordinator = FakeCoordinator {
        results: RefCell::new(vec![Ok(false), Ok(false)]),
    };
    let ledger = ledger(Phase::Halted, "", Some("cancelled"));
    let binding = binding("worker", "hermes-job-worker");
    let id = identity();
    let report = inspect_operator_recovery(
        &coordinator,
        &ledger,
        &RecoveryInspectionRequest {
            identity: &id,
            bindings: [&binding, &binding],
        },
    )
    .unwrap();
    assert!(report.requires_investigation);
    assert!(report.guidance.contains("do not replay"));
}
