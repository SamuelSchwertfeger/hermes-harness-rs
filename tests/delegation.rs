#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]
//! Deterministic delegation tests; no models, network or Git writes.

use hermes_harness::core::delegation::{
    Brief, DelegationError, Finding, OfflineAgent, Phase, begin, delegate, phase, request_approval,
    review,
};

struct FakeAgent {
    identity: String,
    calls: u32,
}

impl FakeAgent {
    fn new(identity: &str) -> Self {
        Self {
            identity: identity.to_owned(),
            calls: 0,
        }
    }
}

impl OfflineAgent for FakeAgent {
    fn run(&mut self, _brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        self.calls += 1;
        let text = if prior.is_some() {
            "Reviewed worker"
        } else {
            "Worker output"
        };
        Ok(Finding {
            agent_id: self.identity.clone(),
            text: text.to_owned(),
        })
    }
}

fn brief() -> Brief {
    Brief {
        task_id: "task-one".to_owned(),
        objective: "Offline example".to_owned(),
        owned_paths: vec!["src/safe.py".to_owned()],
        worker_id: "worker".to_owned(),
        reviewer_id: "reviewer".to_owned(),
    }
}

#[test]
fn complete_offline_handoff() {
    let dir = tempfile::tempdir().unwrap();
    let (tmp, brief) = (dir.path(), brief());
    begin(tmp, &brief).unwrap();
    assert_eq!(phase(tmp, &brief).unwrap(), Phase::Created);
    delegate(tmp, &brief, &mut FakeAgent::new("worker")).unwrap();
    assert_eq!(phase(tmp, &brief).unwrap(), Phase::WorkerDone);
    review(tmp, &brief, &mut FakeAgent::new("reviewer")).unwrap();
    assert_eq!(phase(tmp, &brief).unwrap(), Phase::Reviewed);
    let approval = request_approval(tmp, &brief).unwrap();
    assert_eq!(phase(tmp, &brief).unwrap(), Phase::AwaitingApproval);
    let bytes = std::fs::read(approval).unwrap();
    assert!(
        String::from_utf8(bytes)
            .unwrap()
            .contains("\"approved\":false")
    );
}

#[test]
fn recovery_prevents_worker_reexecution() {
    let dir = tempfile::tempdir().unwrap();
    let (tmp, brief) = (dir.path(), brief());
    begin(tmp, &brief).unwrap();
    let mut worker = FakeAgent::new("worker");
    delegate(tmp, &brief, &mut worker).unwrap();
    assert!(delegate(tmp, &brief, &mut worker).is_err());
    assert_eq!(worker.calls, 1);
}

#[test]
fn review_requires_worker() {
    let dir = tempfile::tempdir().unwrap();
    let (tmp, brief) = (dir.path(), brief());
    begin(tmp, &brief).unwrap();
    assert!(review(tmp, &brief, &mut FakeAgent::new("reviewer")).is_err());
}

#[test]
fn reviewer_must_be_independent() {
    let dir = tempfile::tempdir().unwrap();
    let brief = Brief {
        reviewer_id: "worker".to_owned(),
        ..brief()
    };
    assert!(begin(dir.path(), &brief).is_err());
}

#[test]
fn wrong_identity_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let (tmp, brief) = (dir.path(), brief());
    begin(tmp, &brief).unwrap();
    assert!(delegate(tmp, &brief, &mut FakeAgent::new("intruder")).is_err());
    assert_eq!(phase(tmp, &brief).unwrap(), Phase::Created);
}

#[test]
fn mutated_brief_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (tmp, brief) = (dir.path(), brief());
    begin(tmp, &brief).unwrap();
    let modified = Brief {
        objective: "Modified".to_owned(),
        ..brief
    };
    assert!(phase(tmp, &modified).is_err());
}

#[test]
fn approval_requires_review() {
    let dir = tempfile::tempdir().unwrap();
    let (tmp, brief) = (dir.path(), brief());
    begin(tmp, &brief).unwrap();
    delegate(tmp, &brief, &mut FakeAgent::new("worker")).unwrap();
    assert!(request_approval(tmp, &brief).is_err());
}

#[test]
fn bad_ownership_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let brief = Brief {
        owned_paths: vec!["../secrets".to_owned()],
        ..brief()
    };
    assert!(begin(dir.path(), &brief).is_err());
}
