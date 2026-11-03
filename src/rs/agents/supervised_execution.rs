//! One-shot scripted supervised gateway orchestration over real Git evidence.
//!
//! This module is strictly for offline qualification. A controlled worker double
//! applies changes after gateway receipt; it is NOT a live Hermes executor.

use crate::util::PathLexical as _;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{Map, Value};

use super::supervised_agents::prepare_supervised_plan;
use super::supervised_agents::{SupervisedAgentsError, SupervisedPlan, SupervisedPolicy};
use super::supervised_review_handoff::{
    HandoffSources, ReviewerHandoffError, prepare_reviewer_handoff_once, verify_reviewer_handoff,
};
use super::supervised_reviewer_dispatch::{
    AuditHandoffVerifier, ReviewerDispatchBlockedError, ReviewerDispatchInputs, RuntimeBinding,
    RuntimeQualifier, submit_reviewer_once,
};
use crate::core::delegation::{DelegationError, OfflineAgent};
use crate::gateway::admission_store::{AdmissionStore, AdmissionStoreError};
use crate::gateway::gateway_identity::Submission;
use crate::gateway::gateway_live_admission::{
    LiveAdmissionError, LiveAdmissionRequest, submit_once,
};
use crate::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use crate::gateway::gateway_session_creation::{RpcFailure, SessionRpc};
use crate::gateway::gateway_terminal_reconciliation::{
    TerminalReconciliationError, reconcile_terminal_once,
};
use crate::gateway::scripted_supervised_gateway::ScriptedSupervisedGateway;
use crate::gateway::supervised_gateway_receipts::RoleAdmissions;
use crate::git::worktree_delegation::{
    TaskWorkspace, WorktreeDelegationError, delegate_in_worktree, prepare_worktree,
    request_worktree_approval, review_worktree, verified_audit,
};
use crate::supervised::offline_orchestrator::inspect_offline_task;
use crate::util::{JsonError, PyStr as _, resolve_strict};

/// Supervised demonstration cannot advance safely.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionBlockedError {
    /// Refused by a gate or precondition.
    #[error("{0}")]
    Blocked(String),
    /// Worker evidence identity violation.
    #[error(transparent)]
    Delegation(#[from] DelegationError),
    /// Worktree delegation failure.
    #[error(transparent)]
    Worktree(#[from] WorktreeDelegationError),
    /// Supervised plan failure.
    #[error(transparent)]
    Plan(#[from] SupervisedAgentsError),
    /// Admission ledger failure.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// Live admission failure.
    #[error(transparent)]
    Admission(#[from] LiveAdmissionError),
    /// Terminal reconciliation failure.
    #[error(transparent)]
    Terminal(#[from] TerminalReconciliationError),
    /// Reviewer handoff failure.
    #[error(transparent)]
    Handoff(#[from] ReviewerHandoffError),
    /// Reviewer dispatch or qualifier refusal.
    #[error(transparent)]
    Dispatch(#[from] ReviewerDispatchBlockedError),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Ledger read failure.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

fn blocked<T>(msg: &str) -> Result<T, ExecutionBlockedError> {
    Err(ExecutionBlockedError::Blocked(msg.to_owned()))
}

/// RPC accepted by the scripted flow.
///
/// Python checks `isinstance(rpc, ScriptedSupervisedGateway)`; this trait seam carries the
/// same fact so the refusal of any other gateway stays testable.
pub trait ExecutionRpc: GatewayRpc {
    /// Whether this is the built-in network-free scripted gateway.
    fn is_builtin_scripted(&self) -> bool;
}

impl ExecutionRpc for ScriptedSupervisedGateway {
    fn is_builtin_scripted(&self) -> bool {
        true
    }
}

/// Explicit test-only inputs; caller controls both deterministic agents.
#[derive(Clone)]
pub struct ScriptedExecution<'a> {
    /// Task state root, workspace and brief.
    pub task: TaskWorkspace,
    /// Disposable source repository.
    pub source_repo: PathBuf,
    /// Supervised authorization.
    pub policy: SupervisedPolicy,
    /// Fresh worker submission.
    pub worker_submission: Submission,
    /// Fresh reviewer submission.
    pub reviewer_submission: Submission,
    /// Admission ledger location; must not exist.
    pub ledger_path: PathBuf,
    /// Built-in scripted gateway.
    pub rpc: &'a RefCell<dyn ExecutionRpc + 'a>,
    /// Runtime qualification gate.
    pub qualifier: &'a dyn RuntimeQualifier,
    /// Worker double.
    pub worker: &'a RefCell<dyn OfflineAgent + 'a>,
    /// Reviewer double.
    pub reviewer: &'a RefCell<dyn OfflineAgent + 'a>,
}

/// Successful scripted flow; approval remains unapproved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionResult {
    /// Worker gateway admission identity.
    pub worker_admission_id: String,
    /// Reviewer gateway admission identity.
    pub reviewer_admission_id: String,
    /// Audit digest bound into the reviewer prompt.
    pub audit_sha256: String,
    /// Unapproved approval request document.
    pub approval_request: PathBuf,
}

/// Borrow-per-call view of the shared gateway for the generic RPC seams.
struct RpcHandle<'a>(&'a RefCell<dyn ExecutionRpc + 'a>);

impl GatewayRpc for RpcHandle<'_> {
    fn rpc(
        &mut self,
        method: &str,
        params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        GatewayRpc::rpc(&mut *self.0.borrow_mut(), method, params)
    }
}

impl SessionRpc for RpcHandle<'_> {
    fn rpc(&mut self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcFailure> {
        let params: BTreeMap<String, Value> = params
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        GatewayRpc::rpc(&mut *self.0.borrow_mut(), method, &params)
            .map_err(|error| Box::new(error) as RpcFailure)
    }
}

fn check_identity(config: &ScriptedExecution<'_>) -> Result<(), ExecutionBlockedError> {
    if config.policy.authorized_task_id != config.task.brief.task_id {
        return blocked("Policy task identity mismatch");
    }
    if config.policy.authorized_workspace != config.task.workspace {
        return blocked("Policy workspace mismatch");
    }
    if config.worker_submission.session_id == config.reviewer_submission.session_id {
        return blocked("Worker and reviewer sessions must differ");
    }
    if config.worker_submission.submission_id == config.reviewer_submission.submission_id {
        return blocked("Worker and reviewer submission IDs must differ");
    }
    if std::ptr::addr_eq(config.worker, config.reviewer) {
        return blocked("Worker and reviewer implementations must differ");
    }
    Ok(())
}

fn check(config: &ScriptedExecution<'_>) -> Result<(), ExecutionBlockedError> {
    if !config.rpc.borrow().is_builtin_scripted() {
        return blocked("Only the built-in network-free scripted gateway is permitted");
    }
    check_identity(config)?;
    let admitted = |submission: &Submission| {
        submission
            .admission_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
    };
    if admitted(&config.worker_submission) || admitted(&config.reviewer_submission) {
        return blocked("Only fresh submissions are permitted");
    }
    if config.ledger_path.exists() {
        return blocked("Existing ledger requires read-only inspection, never fresh execution");
    }
    Ok(())
}

struct ExecutionContext<'c, 'a> {
    config: &'c ScriptedExecution<'a>,
    plan: SupervisedPlan,
    store: AdmissionStore,
    ids: RoleAdmissions,
}

fn completed_nonempty(
    outcome: &str,
    response: Option<&str>,
    msg: &str,
) -> Result<(), ExecutionBlockedError> {
    if outcome != "completed" || response.is_none_or(str::is_empty) {
        return blocked(msg);
    }
    Ok(())
}

fn run_worker(context: &ExecutionContext<'_, '_>) -> Result<(), ExecutionBlockedError> {
    let config = context.config;
    let plan = &context.plan;
    let mut rpc = RpcHandle(config.rpc);
    // The same sandbox attestation is required at the worker boundary.
    config.qualifier.require_qualified(&RuntimeBinding {
        task_id: plan.task_id.clone(),
        reviewer_id: plan.worker_id.clone(),
        session_id: config.worker_submission.session_id.clone(),
        workspace: resolve_strict(&config.task.workspace)?,
        audit_sha256: "pre-worker".to_owned(),
    })?;
    submit_once(
        &context.store,
        &LiveAdmissionRequest {
            submission: config.worker_submission.clone(),
            text: plan.worker.prompt.clone(),
        },
        &mut rpc,
    )?;
    let receipt = reconcile_terminal_once(
        &mut rpc,
        &context.store,
        &config.worker_submission.submission_id,
    )?;
    completed_nonempty(
        &receipt.outcome,
        receipt.final_response.as_deref(),
        "Worker receipt is not a completed nonempty response",
    )?;
    // The deterministic double, not the gateway response, edits the Git worktree.
    let worker = delegate_in_worktree(&config.task, &mut *config.worker.borrow_mut())?;
    if worker.agent_id != plan.worker_id {
        return Err(
            DelegationError::Message("Worker evidence identity mismatch".to_owned()).into(),
        );
    }
    verified_audit(&config.task)?;
    Ok(())
}

fn run_reviewer(context: &ExecutionContext<'_, '_>) -> Result<String, ExecutionBlockedError> {
    let config = context.config;
    let plan = &context.plan;
    let mut rpc = RpcHandle(config.rpc);
    let handoff = prepare_reviewer_handoff_once(
        plan,
        &config.task,
        HandoffSources {
            ids: &context.ids,
            store: &context.store,
            rpc: &mut rpc,
        },
    )?;
    verify_reviewer_handoff(&config.task, &handoff)?;
    submit_reviewer_once(ReviewerDispatchInputs {
        task: &config.task,
        handoff: &handoff,
        submission: &config.reviewer_submission,
        store: &context.store,
        rpc: &mut rpc,
        qualifier: config.qualifier,
        verifier: &AuditHandoffVerifier,
    })?;
    let receipt = reconcile_terminal_once(
        &mut rpc,
        &context.store,
        &config.reviewer_submission.submission_id,
    )?;
    completed_nonempty(
        &receipt.outcome,
        receipt.final_response.as_deref(),
        "Reviewer receipt is not a completed nonempty response",
    )?;
    verify_reviewer_handoff(&config.task, &handoff)?;
    let reviewer = review_worktree(&config.task, &mut *config.reviewer.borrow_mut())?;
    if reviewer.agent_id != plan.reviewer_id || reviewer.text.py_strip().is_empty() {
        return blocked("Independent reviewer finding missing");
    }
    Ok(handoff.audit_sha256)
}

/// Run one offline trace, never resume or replay uncertain submissions.
///
/// # Errors
/// Scripted admission, agent execution, worktree audit, or evidence persistence fails.
pub fn execute_scripted_once(
    config: &ScriptedExecution<'_>,
) -> Result<ExecutionResult, ExecutionBlockedError> {
    check(config)?;
    prepare_worktree(&config.task, &config.source_repo)?;
    let plan = prepare_supervised_plan(&config.task.brief, &config.policy)?;
    let context = ExecutionContext {
        config,
        plan,
        store: AdmissionStore::new(&config.ledger_path)?,
        ids: RoleAdmissions {
            task_id: config.task.brief.task_id.clone(),
            worker_submission_id: config.worker_submission.submission_id.clone(),
            reviewer_submission_id: config.reviewer_submission.submission_id.clone(),
        },
    };
    run_worker(&context)?;
    let audit_sha256 = run_reviewer(&context)?;
    let approval = request_worktree_approval(&config.task)?;
    let worker_record = context.store.get(&config.worker_submission.submission_id)?;
    let reviewer_record = context
        .store
        .get(&config.reviewer_submission.submission_id)?;
    let (Some(worker_id), Some(reviewer_id)) = (
        worker_record.and_then(|record| record.admission_id),
        reviewer_record.and_then(|record| record.admission_id),
    ) else {
        return blocked("Missing durable admissions after reconciliation");
    };
    Ok(ExecutionResult {
        worker_admission_id: worker_id,
        reviewer_admission_id: reviewer_id,
        audit_sha256,
        approval_request: approval,
    })
}

/// One ledger row of a read-only inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InspectedAdmission {
    /// Client submission identity.
    pub submission_id: String,
    /// Local ledger state.
    pub state: String,
    /// Gateway admission identity, if known.
    pub admission_id: Option<String>,
}

/// Read-only progress of a scripted task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScriptedInspection {
    /// Task identity.
    pub task_id: String,
    /// Offline orchestration phase value.
    pub phase: String,
    /// `"missing"` when no ledger file exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger: Option<String>,
    /// Up to 100 ledger rows ordered by submission.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admissions: Option<Vec<InspectedAdmission>>,
}

fn read_rows(ledger_path: &Path) -> Result<Vec<InspectedAdmission>, ExecutionBlockedError> {
    // Opened explicitly read-only: do not construct AdmissionStore, whose
    // initialization may create the database or change its permissions.
    let connection = Connection::open_with_flags(ledger_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = connection.prepare(
        "SELECT submission_id, local_state, admission_id FROM admissions \
         ORDER BY submission_id LIMIT 100",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(InspectedAdmission {
            submission_id: row.get(0)?,
            state: row.get(1)?,
            admission_id: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Read-only progress inspection; never invokes gateway or dispatch.
///
/// # Errors
/// Scripted admission, agent execution, worktree audit, or evidence persistence fails.
pub fn inspect_scripted(
    task: &TaskWorkspace,
    ledger_path: &Path,
) -> Result<ScriptedInspection, ExecutionBlockedError> {
    let status = inspect_offline_task(task)?;
    let task_id = task.brief.task_id.clone();
    let phase = status.state.as_str().to_owned();
    if !ledger_path.is_file() || ledger_path.is_symlink_lexical() {
        return Ok(ScriptedInspection {
            task_id,
            phase,
            ledger: Some("missing".to_owned()),
            admissions: None,
        });
    }
    Ok(ScriptedInspection {
        task_id,
        phase,
        ledger: None,
        admissions: Some(read_rows(ledger_path)?),
    })
}

impl std::fmt::Debug for ScriptedExecution<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedExecution")
            .field("task_id", &self.task.brief.task_id)
            .field("workspace", &self.task.workspace)
            .field("source_repo", &self.source_repo)
            .field("policy", &self.policy)
            .field("worker_submission", &self.worker_submission)
            .field("reviewer_submission", &self.reviewer_submission)
            .field("ledger_path", &self.ledger_path)
            .finish_non_exhaustive()
    }
}
