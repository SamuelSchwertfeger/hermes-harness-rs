#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline orchestration tests; no provider, network or process execution.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use hermes_harness::agents::supervised_agents::{
    SupervisedPlan, SupervisedPolicy, prepare_supervised_plan,
};
use hermes_harness::agents::supervised_review_handoff::ReviewerHandoff;
use hermes_harness::core::delegation::Brief;
use hermes_harness::gateway::admission_store::AdmissionStore;
use hermes_harness::gateway::gateway_terminal_reconciliation::TerminalEvidence;
use hermes_harness::gateway::scripted_supervised_gateway::ScriptedSupervisedGateway;
use hermes_harness::gateway::supervised_gateway_receipts::{ReceiptSources, RoleCompletion};
use hermes_harness::git::worktree_delegation::TaskWorkspace;
use hermes_harness::supervised::live_human_decision::DecisionEvidence;
use hermes_harness::supervised::live_supervised_finalization::{
    FinalizationBlockedError, FinalizationInputs, FinalizationOps, finalize_with,
};
use hermes_harness::supervised::live_supervised_recovery::{
    Phase, RecoveryCheckpoint, RecoveryIdentity, RecoveryLedger, evidence_digest,
};
use tempfile::TempDir;

struct Fixture {
    _tmp: TempDir,
    plan: SupervisedPlan,
    task: TaskWorkspace,
    handoff: ReviewerHandoff,
    admissions: AdmissionStore,
    other: AdmissionStore,
    recovery: RecoveryLedger,
    identity: RecoveryIdentity,
    output_dir: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let output_dir = tmp.path().join("out");
    std::fs::create_dir(&output_dir).unwrap();
    let brief = Brief {
        task_id: "task".to_owned(),
        objective: "Test finalization".to_owned(),
        owned_paths: vec!["src/".to_owned()],
        worker_id: "worker".to_owned(),
        reviewer_id: "reviewer".to_owned(),
    };
    let policy = SupervisedPolicy::new(&brief.task_id, &workspace);
    Fixture {
        plan: prepare_supervised_plan(&brief, &policy).unwrap(),
        task: TaskWorkspace {
            state_root: tmp.path().join("state"),
            workspace,
            brief,
        },
        handoff: ReviewerHandoff {
            task_id: "task".to_owned(),
            reviewer_id: "reviewer".to_owned(),
            worker_submission_id: "worker".to_owned(),
            worker_admission_id: "adm".to_owned(),
            audit_sha256: "a".repeat(64),
            reviewer_prompt: "prompt".to_owned(),
        },
        admissions: AdmissionStore::new(&tmp.path().join("admissions.sqlite")).unwrap(),
        other: AdmissionStore::new(&tmp.path().join("other.sqlite")).unwrap(),
        recovery: RecoveryLedger::new(&tmp.path().join("recovery.db")).unwrap(),
        identity: RecoveryIdentity::new("task", "worker", "reviewer").unwrap(),
        output_dir,
        _tmp: tmp,
    }
}

fn inputs<'a>(
    fx: &'a Fixture,
    rpc: &'a mut ScriptedSupervisedGateway,
    receipts_store: &'a AdmissionStore,
) -> FinalizationInputs<'a, ScriptedSupervisedGateway> {
    FinalizationInputs {
        plan: &fx.plan,
        task: &fx.task,
        handoff: &fx.handoff,
        admissions: &fx.admissions,
        receipts: ReceiptSources {
            store: receipts_store,
            rpc,
        },
        recovery: &fx.recovery,
        identity: &fx.identity,
        output_dir: &fx.output_dir,
    }
}

fn checkpoint(phase: Phase) -> RecoveryCheckpoint {
    RecoveryCheckpoint {
        phase,
        evidence_sha256: String::new(),
        halted_reason: None,
    }
}

fn terminal() -> TerminalEvidence<()> {
    TerminalEvidence {
        submission_id: "s".to_owned(),
        admission_id: "a".to_owned(),
        outcome: "completed".to_owned(),
        execution_generation: None,
        final_response: None,
        recorded: (),
    }
}

/// Stand-in for the patched Python collaborators, with call counters.
#[derive(Default)]
struct FakeOps {
    advances: RefCell<VecDeque<Phase>>,
    advance_calls: Cell<usize>,
    reconcile_calls: Cell<usize>,
    write_calls: Cell<usize>,
    pending_calls: Cell<usize>,
}

impl FinalizationOps<ScriptedSupervisedGateway> for FakeOps {
    type Record = ();

    fn verify_handoff(
        &self,
        _inputs: &FinalizationInputs<'_, ScriptedSupervisedGateway>,
    ) -> Result<(), FinalizationBlockedError> {
        Ok(())
    }

    fn advance_evidence(
        &self,
        _inputs: &FinalizationInputs<'_, ScriptedSupervisedGateway>,
        _phase: Phase,
    ) -> Result<RecoveryCheckpoint, FinalizationBlockedError> {
        self.advance_calls.set(self.advance_calls.get() + 1);
        Ok(checkpoint(self.advances.borrow_mut().pop_front().unwrap()))
    }

    fn reconcile_roles(
        &self,
        _inputs: &mut FinalizationInputs<'_, ScriptedSupervisedGateway>,
    ) -> Result<RoleCompletion<()>, FinalizationBlockedError> {
        self.reconcile_calls.set(self.reconcile_calls.get() + 1);
        Ok(RoleCompletion {
            worker: terminal(),
            reviewer: terminal(),
        })
    }

    fn inspect_pending(
        &self,
        _path: &Path,
        _identity: &RecoveryIdentity,
    ) -> Result<String, FinalizationBlockedError> {
        Ok("a".repeat(64))
    }

    fn request_decision(
        &self,
        _inputs: &FinalizationInputs<'_, ScriptedSupervisedGateway>,
        _evidence: &DecisionEvidence<()>,
    ) -> Result<PathBuf, FinalizationBlockedError> {
        self.write_calls.set(self.write_calls.get() + 1);
        Ok(PathBuf::new())
    }

    fn checkpoint_pending(
        &self,
        _inputs: &FinalizationInputs<'_, ScriptedSupervisedGateway>,
        _path: &Path,
    ) -> Result<RecoveryCheckpoint, FinalizationBlockedError> {
        self.pending_calls.set(self.pending_calls.get() + 1);
        Ok(checkpoint(Phase::PendingHuman))
    }
}

fn advance_to(fx: &Fixture, transitions: &[(Phase, Phase)]) {
    fx.recovery.initialize(&fx.identity).unwrap();
    for transition in transitions {
        let digest = evidence_digest(&format!("{transition:?}")).unwrap();
        fx.recovery
            .advance(&fx.identity, *transition, &digest)
            .unwrap();
    }
}

const STEPS: [(Phase, Phase); 4] = [
    (Phase::Planned, Phase::WorkerAdmitted),
    (Phase::WorkerAdmitted, Phase::WorkerAudited),
    (Phase::WorkerAudited, Phase::ReviewerAdmitted),
    (Phase::ReviewerAdmitted, Phase::PendingHuman),
];

#[test]
fn complete_offline_finalization() {
    let fx = fixture();
    let ops = FakeOps::default();
    *ops.advances.borrow_mut() = VecDeque::from([
        Phase::WorkerAdmitted,
        Phase::WorkerAudited,
        Phase::ReviewerAdmitted,
    ]);
    let mut rpc = ScriptedSupervisedGateway::default();
    let result = finalize_with(&mut inputs(&fx, &mut rpc, &fx.admissions), &ops).unwrap();
    assert_eq!(result.phase, Phase::PendingHuman);
    assert_eq!(ops.advance_calls.get(), 3);
    assert_eq!(ops.reconcile_calls.get(), 1);
    assert_eq!(ops.write_calls.get(), 1);
    assert_eq!(ops.pending_calls.get(), 1);
}

#[test]
fn halted_checkpoint_does_not_reconcile() {
    let fx = fixture();
    fx.recovery.initialize(&fx.identity).unwrap();
    fx.recovery.halt(&fx.identity, "operator denied").unwrap();
    let ops = FakeOps::default();
    let mut rpc = ScriptedSupervisedGateway::default();
    let err = finalize_with(&mut inputs(&fx, &mut rpc, &fx.admissions), &ops).unwrap_err();
    assert!(err.to_string().contains("halted"));
    assert_eq!(ops.reconcile_calls.get(), 0);
}

#[test]
fn missing_pending_artifact_cannot_be_regenerated() {
    let fx = fixture();
    advance_to(&fx, &STEPS);
    let ops = FakeOps::default();
    let mut rpc = ScriptedSupervisedGateway::default();
    let err = finalize_with(&mut inputs(&fx, &mut rpc, &fx.admissions), &ops).unwrap_err();
    assert!(err.to_string().contains("missing"));
    assert_eq!(ops.write_calls.get(), 0);
}

#[test]
fn wrong_ledger_blocks_before_gateway() {
    let fx = fixture();
    let ops = FakeOps::default();
    let mut rpc = ScriptedSupervisedGateway::default();
    let err = finalize_with(&mut inputs(&fx, &mut rpc, &fx.other), &ops).unwrap_err();
    assert!(err.to_string().contains("authoritative"));
    assert_eq!(ops.reconcile_calls.get(), 0);
    assert!(rpc.calls.is_empty());
}

#[test]
fn existing_decision_never_writes_twice() {
    let fx = fixture();
    let path = fx.output_dir.join("human-decision-request.json");
    std::fs::write(&path, "evidence").unwrap();
    let ops = FakeOps::default();
    let mut rpc = ScriptedSupervisedGateway::default();
    let err = finalize_with(&mut inputs(&fx, &mut rpc, &fx.admissions), &ops).unwrap_err();
    assert!(err.to_string().contains("conflicts"));
    assert_eq!(ops.write_calls.get(), 0);
}

#[test]
fn pending_request_recovered_without_rewrite() {
    let fx = fixture();
    advance_to(&fx, &STEPS[..3]);
    let path = fx.output_dir.join("human-decision-request.json");
    std::fs::write(&path, "existing decision").unwrap();
    let ops = FakeOps::default();
    let mut rpc = ScriptedSupervisedGateway::default();
    let result = finalize_with(&mut inputs(&fx, &mut rpc, &fx.admissions), &ops).unwrap();
    assert_eq!(result.phase, Phase::PendingHuman);
    assert_eq!(ops.write_calls.get(), 0);
    assert_eq!(ops.pending_calls.get(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing decision");
}
