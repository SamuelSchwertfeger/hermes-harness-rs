//! Durably request human review from authenticated receipts and Git audit.
//!
//! Never approves, merges, dispatches, or replays a gateway job.

use crate::util::PathLexical as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::live_reviewer_verdict::{
    ReviewerVerdict, ReviewerVerdictError, VerdictBinding, parse_reviewer_verdict,
};
use crate::agents::supervised_review_handoff::{
    ReviewerHandoff, ReviewerHandoffError, verify_reviewer_handoff,
};
use crate::core::delegation::create_new_private;
use crate::gateway::gateway_terminal_reconciliation::TerminalEvidence;
use crate::git::worktree_delegation::TaskWorkspace;
use crate::util::{canonical_json, sha256_hex};

/// Evidence or durable decision request is unsafe or inconsistent.
#[derive(Debug, thiserror::Error)]
pub enum HumanDecisionError {
    /// Evidence or output location violates a decision rule.
    #[error("{0}")]
    Refused(String),
    /// The handoff verifier failed; propagated unchanged.
    #[error(transparent)]
    Handoff(#[from] ReviewerHandoffError),
    /// The reviewer response is not a valid bound verdict.
    #[error(transparent)]
    Verdict(#[from] ReviewerVerdictError),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

fn refuse<T>(msg: &str) -> Result<T, HumanDecisionError> {
    Err(HumanDecisionError::Refused(msg.to_owned()))
}

/// Existing verified terminal receipts, never synthetic review correctness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionEvidence<R> {
    /// Worker terminal evidence.
    pub worker: TerminalEvidence<R>,
    /// Reviewer terminal evidence.
    pub reviewer: TerminalEvidence<R>,
}

/// Input bundle for a one-shot, human-only approval request.
#[derive(Debug, Clone)]
pub struct DecisionRequest<'a, R> {
    /// Audited worktree.
    pub task: &'a TaskWorkspace,
    /// Immutable reviewer handoff.
    pub handoff: &'a ReviewerHandoff,
    /// Terminal receipts of both roles.
    pub receipts: &'a DecisionEvidence<R>,
    /// Existing output directory.
    pub output_dir: &'a Path,
}

fn require_terminals<'a, R>(
    handoff: &ReviewerHandoff,
    receipts: &'a DecisionEvidence<R>,
) -> Result<&'a str, HumanDecisionError> {
    let worker = &receipts.worker;
    let reviewer = &receipts.reviewer;
    if worker.submission_id != handoff.worker_submission_id {
        return refuse("Worker receipt does not match handoff");
    }
    if worker.admission_id != handoff.worker_admission_id {
        return refuse("Worker admission does not match handoff");
    }
    if worker.outcome != "completed" || reviewer.outcome != "completed" {
        return refuse("Both gateway jobs require successful terminal outcomes");
    }
    let worker_response = worker.final_response.as_deref().unwrap_or_default();
    let reviewer_response = reviewer.final_response.as_deref().unwrap_or_default();
    if worker_response.is_empty() || reviewer_response.is_empty() {
        return refuse("Both roles require nonempty terminal responses");
    }
    if reviewer.submission_id == worker.submission_id {
        return refuse("Reviewer and worker submission identities must differ");
    }
    if reviewer.admission_id == worker.admission_id {
        return refuse("Reviewer and worker admission identities must differ");
    }
    Ok(reviewer_response)
}

#[derive(Serialize)]
struct RequestDocument<'a> {
    schema_version: u8,
    task_id: &'a str,
    worker_submission_id: &'a str,
    worker_admission_id: &'a str,
    reviewer_submission_id: &'a str,
    reviewer_admission_id: &'a str,
    audit_sha256: &'a str,
    reviewer_verdict: &'a ReviewerVerdict,
    worker_response_sha256: String,
    reviewer_response_sha256: String,
    approved: bool,
    human_decision: &'a str,
}

fn request_document<'a, R>(
    handoff: &'a ReviewerHandoff,
    receipts: &'a DecisionEvidence<R>,
    verdict: &'a ReviewerVerdict,
) -> RequestDocument<'a> {
    let digest = |evidence: &TerminalEvidence<R>| {
        sha256_hex(
            evidence
                .final_response
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        )
    };
    RequestDocument {
        schema_version: 1,
        task_id: &handoff.task_id,
        worker_submission_id: &handoff.worker_submission_id,
        worker_admission_id: &handoff.worker_admission_id,
        reviewer_submission_id: &receipts.reviewer.submission_id,
        reviewer_admission_id: &receipts.reviewer.admission_id,
        audit_sha256: &handoff.audit_sha256,
        reviewer_verdict: verdict,
        worker_response_sha256: digest(&receipts.worker),
        reviewer_response_sha256: digest(&receipts.reviewer),
        approved: false,
        human_decision: "pending",
    }
}

/// Verify immutable handoff, validate reviewer JSON and write once atomically.
///
/// Caller owns output directory; an existing file is never overwritten or retried.
///
/// # Errors
/// Required terminal or audit evidence is invalid, or decision persistence fails.
pub fn request_human_decision_once<R>(
    request: &DecisionRequest<'_, R>,
) -> Result<PathBuf, HumanDecisionError> {
    request_human_decision_verified(request, || {
        verify_reviewer_handoff(request.task, request.handoff)
    })
}

/// Same as [`request_human_decision_once`] with an injected handoff verifier.
///
/// The verifier runs first; its failure aborts before anything is written.
///
/// # Security
/// Production callers must use [`request_human_decision_once`], because a permissive
/// `verify` skips the human-approval evidence checks.
///
/// # Errors
/// Required terminal or audit evidence is invalid, or decision persistence fails.
pub(crate) fn request_human_decision_verified<R>(
    request: &DecisionRequest<'_, R>,
    verify: impl FnOnce() -> Result<(), ReviewerHandoffError>,
) -> Result<PathBuf, HumanDecisionError> {
    let handoff = request.handoff;
    let receipts = request.receipts;
    let output_dir = request.output_dir;
    verify()?;
    let response = require_terminals(handoff, receipts)?;
    let verdict = parse_reviewer_verdict(
        response,
        &VerdictBinding {
            task_id: handoff.task_id.clone(),
            worker_submission_id: handoff.worker_submission_id.clone(),
            audit_sha256: handoff.audit_sha256.clone(),
        },
    )?;
    if output_dir.is_symlink_lexical() || !output_dir.is_dir() {
        return refuse("Decision output directory must exist and cannot be a symlink");
    }
    let path = output_dir.join("human-decision-request.json");
    let mut data = canonical_json(&request_document(handoff, receipts, &verdict))?.into_bytes();
    data.push(b'\n');
    let mut stream = match create_new_private(&path) {
        Ok(stream) => stream,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return refuse("Decision evidence already exists; do not overwrite");
        }
        Err(error) => return Err(error.into()),
    };
    stream.write_all(&data)?;
    stream.flush()?;
    stream.sync_all()?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    #![allow(
        missing_docs,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::pedantic,
        clippy::nursery
    )]
    use std::cell::Cell;
    use std::path::Path;

    use crate::agents::supervised_review_handoff::{ReviewerHandoff, ReviewerHandoffError};
    use crate::core::delegation::Brief;
    use crate::gateway::gateway_terminal_reconciliation::TerminalEvidence;
    use crate::git::worktree_delegation::TaskWorkspace;
    use crate::supervised::live_human_decision::{
        DecisionEvidence, DecisionRequest, HumanDecisionError, request_human_decision_verified,
    };
    use serde_json::{Value, json};

    fn receipt(submission_id: &str, admission_id: &str, response: &str) -> TerminalEvidence<()> {
        TerminalEvidence {
            submission_id: submission_id.to_owned(),
            admission_id: admission_id.to_owned(),
            outcome: "completed".to_owned(),
            execution_generation: None,
            final_response: Some(response.to_owned()),
            recorded: (),
        }
    }

    fn evidence() -> DecisionEvidence<()> {
        let response = json!({
            "task_id": "task",
            "worker_submission_id": "worker",
            "audit_sha256": "a".repeat(64),
            "decision": "changes_requested",
            "findings": ["Add a regression test"],
        })
        .to_string();
        DecisionEvidence {
            worker: receipt("worker", "worker-adm", "patched code"),
            reviewer: receipt("reviewer", "reviewer-adm", &response),
        }
    }

    fn handoff() -> ReviewerHandoff {
        ReviewerHandoff {
            task_id: "task".to_owned(),
            reviewer_id: "reviewer-agent".to_owned(),
            worker_submission_id: "worker".to_owned(),
            worker_admission_id: "worker-adm".to_owned(),
            audit_sha256: "a".repeat(64),
            reviewer_prompt: "Audit this worktree".to_owned(),
        }
    }

    fn task() -> TaskWorkspace {
        TaskWorkspace {
            state_root: "state".into(),
            workspace: "workspace".into(),
            brief: Brief {
                task_id: "task".to_owned(),
                objective: "demo".to_owned(),
                owned_paths: vec!["src/".to_owned()],
                worker_id: "worker-agent".to_owned(),
                reviewer_id: "reviewer-agent".to_owned(),
            },
        }
    }

    fn entries(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn request_written_once_without_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let (task, handoff, receipts) = (task(), handoff(), evidence());
        let request = DecisionRequest {
            task: &task,
            handoff: &handoff,
            receipts: &receipts,
            output_dir: tmp.path(),
        };
        let calls = Cell::new(0);
        let verify = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let path = request_human_decision_verified(&request, verify).unwrap();
        assert_eq!(calls.get(), 1);
        let document: Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(document["approved"], Value::Bool(false));
        assert_eq!(document["human_decision"], "pending");
        assert_eq!(
            document["reviewer_verdict"]["decision"],
            "changes_requested"
        );
        let err = request_human_decision_verified(&request, || Ok(())).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn reviewer_identity_collision_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let (task, handoff, mut receipts) = (task(), handoff(), evidence());
        "worker".clone_into(&mut receipts.reviewer.submission_id);
        let request = DecisionRequest {
            task: &task,
            handoff: &handoff,
            receipts: &receipts,
            output_dir: tmp.path(),
        };
        let err = request_human_decision_verified(&request, || Ok(())).unwrap_err();
        assert!(matches!(err, HumanDecisionError::Refused(_)));
        assert!(err.to_string().contains("must differ"));
        assert_eq!(entries(tmp.path()), 0);
    }

    #[test]
    fn handoff_verifier_failure_aborts_before_write() {
        let tmp = tempfile::tempdir().unwrap();
        let (task, handoff, receipts) = (task(), handoff(), evidence());
        let request = DecisionRequest {
            task: &task,
            handoff: &handoff,
            receipts: &receipts,
            output_dir: tmp.path(),
        };
        let err = request_human_decision_verified(&request, || {
            Err(ReviewerHandoffError::Handoff("tamper".to_owned()))
        })
        .unwrap_err();
        assert!(matches!(err, HumanDecisionError::Handoff(_)));
        assert!(err.to_string().contains("tamper"));
        assert_eq!(entries(tmp.path()), 0);
    }
}
