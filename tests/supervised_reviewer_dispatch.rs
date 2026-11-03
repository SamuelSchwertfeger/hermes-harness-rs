#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::cell::RefCell;
use std::path::Path;

use hermes_harness::agents::supervised_review_handoff::{ReviewerHandoff, ReviewerHandoffError};
use hermes_harness::agents::supervised_reviewer_dispatch::{
    AuditHandoffVerifier, DisabledRuntimeQualifier, HandoffVerifier, ReviewerDispatchBlockedError,
    ReviewerDispatchInputs, RuntimeBinding, RuntimeQualifier, submit_reviewer_once,
};
use hermes_harness::core::delegation::Brief;
use hermes_harness::gateway::admission_store::{AdmissionStore, LocalState};
use hermes_harness::gateway::gateway_identity::{GatewayIdentityError, Submission};
use hermes_harness::gateway::gateway_live_admission::{LiveAdmissionError, LiveAdmissionResult};
use hermes_harness::gateway::gateway_session_creation::{RpcFailure, SessionRpc};
use hermes_harness::git::worktree_delegation::TaskWorkspace;
use serde_json::{Map, Value, json};

#[derive(Default)]
struct AllowForScriptedTest {
    calls: RefCell<Vec<RuntimeBinding>>,
}

impl RuntimeQualifier for AllowForScriptedTest {
    fn require_qualified(
        &self,
        binding: &RuntimeBinding,
    ) -> Result<(), ReviewerDispatchBlockedError> {
        self.calls.borrow_mut().push(binding.clone());
        Ok(())
    }
}

struct AcceptAudit;

impl HandoffVerifier for AcceptAudit {
    fn verify(&self, _: &TaskWorkspace, _: &ReviewerHandoff) -> Result<(), ReviewerHandoffError> {
        Ok(())
    }
}

struct RejectAudit;

impl HandoffVerifier for RejectAudit {
    fn verify(&self, _: &TaskWorkspace, _: &ReviewerHandoff) -> Result<(), ReviewerHandoffError> {
        Err(ReviewerHandoffError::Handoff("Audit mismatch".to_owned()))
    }
}

struct ScriptedRpc {
    calls: Vec<String>,
    fail: bool,
}

impl ScriptedRpc {
    const fn new(fail: bool) -> Self {
        Self {
            calls: Vec::new(),
            fail,
        }
    }
}

impl SessionRpc for ScriptedRpc {
    fn rpc(&mut self, method: &str, _params: &Map<String, Value>) -> Result<Value, RpcFailure> {
        self.calls.push(method.to_owned());
        if self.fail && method == "prompt.submit" {
            return Err(std::io::Error::other("unconfirmed transport failure").into());
        }
        Ok(json!({"admission_id": "review-admission", "status": "queued"}))
    }
}

struct Fixture {
    task: TaskWorkspace,
    handoff: ReviewerHandoff,
    submission: Submission,
    store: AdmissionStore,
}

fn fixture(tmp: &Path) -> Fixture {
    let workspace = tmp.join("worktree");
    std::fs::create_dir(&workspace).unwrap();
    Fixture {
        task: TaskWorkspace {
            state_root: tmp.join("state"),
            workspace,
            brief: Brief {
                task_id: "task".to_owned(),
                objective: "demo".to_owned(),
                owned_paths: vec!["src/a.py".to_owned()],
                worker_id: "worker".to_owned(),
                reviewer_id: "reviewer".to_owned(),
            },
        },
        handoff: ReviewerHandoff {
            task_id: "task".to_owned(),
            reviewer_id: "reviewer".to_owned(),
            worker_submission_id: "worker-sub".to_owned(),
            worker_admission_id: "worker-admission".to_owned(),
            audit_sha256: "abc".to_owned(),
            reviewer_prompt: "Review evidence".to_owned(),
        },
        submission: Submission::new("review-session", "review-sub"),
        store: AdmissionStore::new(&tmp.join("ledger.sqlite")).unwrap(),
    }
}

fn dispatch(
    fx: &Fixture,
    rpc: &mut ScriptedRpc,
    qualifier: &dyn RuntimeQualifier,
    verifier: &dyn HandoffVerifier,
) -> Result<LiveAdmissionResult, ReviewerDispatchBlockedError> {
    submit_reviewer_once(ReviewerDispatchInputs {
        task: &fx.task,
        handoff: &fx.handoff,
        submission: &fx.submission,
        store: &fx.store,
        rpc,
        qualifier,
        verifier,
    })
}

#[test]
fn disabled_gate_prevents_submission() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture(tmp.path());
    let mut rpc = ScriptedRpc::new(false);
    let err = dispatch(&fx, &mut rpc, &DisabledRuntimeQualifier, &AcceptAudit).unwrap_err();
    assert!(matches!(err, ReviewerDispatchBlockedError::Blocked(_)));
    assert!(err.to_string().contains("disabled"));
    assert_eq!(rpc.calls, Vec::<String>::new());
    assert!(fx.store.get("review-sub").unwrap().is_none());
}

#[test]
fn qualified_scripted_submission_once() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture(tmp.path());
    let mut rpc = ScriptedRpc::new(false);
    let qualifier = AllowForScriptedTest::default();
    let result = dispatch(&fx, &mut rpc, &qualifier, &AcceptAudit).unwrap();
    assert_eq!(result.admission_id, "review-admission");
    assert_eq!(rpc.calls, ["prompt.submit", "prompt.receipt"]);
    assert_eq!(qualifier.calls.borrow()[0].session_id, "review-session");
    let err = dispatch(&fx, &mut rpc, &qualifier, &AcceptAudit).unwrap_err();
    assert!(matches!(err, ReviewerDispatchBlockedError::Blocked(_)));
    assert!(err.to_string().contains("already recorded"));
    assert_eq!(rpc.calls, ["prompt.submit", "prompt.receipt"]);
}

#[test]
fn uncertain_submit_never_retries() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture(tmp.path());
    let mut rpc = ScriptedRpc::new(true);
    let qualifier = AllowForScriptedTest::default();
    let err = dispatch(&fx, &mut rpc, &qualifier, &AcceptAudit).unwrap_err();
    assert!(matches!(
        err,
        ReviewerDispatchBlockedError::Admission(LiveAdmissionError::Identity(
            GatewayIdentityError::OutcomeUnknown { .. }
        ))
    ));
    let record = fx.store.get("review-sub").unwrap().unwrap();
    assert_eq!(record.local_state, LocalState::OutcomeUnknown);
    let err = dispatch(&fx, &mut rpc, &qualifier, &AcceptAudit).unwrap_err();
    assert!(matches!(err, ReviewerDispatchBlockedError::Blocked(_)));
    assert_eq!(rpc.calls, ["prompt.submit"]);
}

#[test]
fn rejected_stale_audit_never_calls_qualifier() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture(tmp.path());
    let mut rpc = ScriptedRpc::new(false);
    let qualifier = AllowForScriptedTest::default();
    let err = dispatch(&fx, &mut rpc, &qualifier, &RejectAudit).unwrap_err();
    assert!(err.to_string().contains("Audit mismatch"));
    assert!(qualifier.calls.borrow().is_empty());
    assert_eq!(rpc.calls, Vec::<String>::new());
}

#[test]
fn default_verifier_fails_closed_without_audit_record() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = fixture(tmp.path());
    assert!(AuditHandoffVerifier.verify(&fx.task, &fx.handoff).is_err());
}
