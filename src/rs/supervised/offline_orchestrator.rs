//! One-shot offline worker/reviewer coordinator with durable fail-closed fences.
//!
//! Only caller-supplied, controlled offline agents are supported. No live runtime,
//! provider, network, merge, approval grant, or automatic recovery is performed.

use crate::util::PathLexical as _;
use std::cell::RefCell;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::core::delegation::{DelegationError, Finding, OfflineAgent, Phase, phase};
use crate::core::task_reservation::{RecoveryState, reconcile};
use crate::git::worktree_delegation::{
    TaskWorkspace, WorktreeDelegationError, delegate_in_worktree, prepare_worktree,
    request_worktree_approval, review_worktree, verified_audit,
};

/// Two deliberately supplied offline agent implementations.
///
/// Identity is object identity: handing the same cell to both roles is refused.
#[derive(Clone, Copy)]
pub struct OfflineAgents<'a> {
    /// Worker implementation.
    pub worker: &'a RefCell<dyn OfflineAgent + 'a>,
    /// Reviewer implementation.
    pub reviewer: &'a RefCell<dyn OfflineAgent + 'a>,
}

impl<'a> OfflineAgents<'a> {
    /// Pair a worker and a reviewer.
    #[must_use]
    pub const fn new(
        worker: &'a RefCell<dyn OfflineAgent + 'a>,
        reviewer: &'a RefCell<dyn OfflineAgent + 'a>,
    ) -> Self {
        Self { worker, reviewer }
    }

    /// Whether both roles are the same implementation object (Python `is`).
    #[must_use]
    pub fn same_agent(&self) -> bool {
        std::ptr::addr_eq(self.worker, self.reviewer)
    }
}

impl fmt::Debug for OfflineAgents<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OfflineAgents").finish_non_exhaustive()
    }
}

/// Result of fresh orchestration or read-only inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrchestrationState {
    /// `awaiting_approval`
    AwaitingApproval,
    /// `reserved`
    Reserved,
    /// `preparing_uncertain`
    Preparing,
    /// `prepared`
    Prepared,
    /// `dispatch_uncertain`
    DispatchUncertain,
    /// `worker_recorded`
    WorkerRecorded,
    /// `review_uncertain`
    ReviewUncertain,
    /// `reviewed`
    Reviewed,
    /// `manual_intervention`
    ManualIntervention,
}

impl OrchestrationState {
    /// The Python enum value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingApproval => "awaiting_approval",
            Self::Reserved => "reserved",
            Self::Preparing => "preparing_uncertain",
            Self::Prepared => "prepared",
            Self::DispatchUncertain => "dispatch_uncertain",
            Self::WorkerRecorded => "worker_recorded",
            Self::ReviewUncertain => "review_uncertain",
            Self::Reviewed => "reviewed",
            Self::ManualIntervention => "manual_intervention",
        }
    }
}

impl fmt::Display for OrchestrationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<RecoveryState> for OrchestrationState {
    fn from(recovery: RecoveryState) -> Self {
        match recovery {
            RecoveryState::Reserved => Self::Reserved,
            RecoveryState::Preparing => Self::Preparing,
            RecoveryState::Prepared => Self::Prepared,
            RecoveryState::DispatchUncertain => Self::DispatchUncertain,
            RecoveryState::WorkerRecorded => Self::WorkerRecorded,
            RecoveryState::ManualIntervention => Self::ManualIntervention,
        }
    }
}

/// Non-authorizing task status and local evidence references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationResult {
    /// Orchestration state.
    pub state: OrchestrationState,
    /// Task identifier.
    pub task_id: String,
    /// Worker finding, when fresh orchestration ran.
    pub worker: Option<Finding>,
    /// Reviewer finding, when fresh orchestration ran.
    pub reviewer: Option<Finding>,
    /// Unapproved approval request document.
    pub approval_request: Option<PathBuf>,
}

impl OrchestrationResult {
    fn bare(state: OrchestrationState, task_id: &str) -> Self {
        Self {
            state,
            task_id: task_id.to_owned(),
            worker: None,
            reviewer: None,
            approval_request: None,
        }
    }
}

/// Run exactly one fresh offline task; errors are terminal, never retried.
///
/// Any error leaves existing task evidence/worktree intact for manual inspection.
/// Reinvoking this function for a reserved task fails at reservation creation.
///
/// # Errors
/// Task preparation, injected agent execution, review, or state persistence fails.
pub fn run_offline_task(
    task: &TaskWorkspace,
    source_repo: &Path,
    agents: &OfflineAgents<'_>,
) -> Result<OrchestrationResult, WorktreeDelegationError> {
    if task.brief.worker_id == task.brief.reviewer_id || agents.same_agent() {
        return Err(DelegationError::Message(
            "Worker and reviewer must have distinct identities and implementations".to_owned(),
        )
        .into());
    }
    prepare_worktree(task, source_repo)?;
    let worker = delegate_in_worktree(task, &mut *agents.worker.borrow_mut())?;
    verified_audit(task)?;
    let reviewer = review_worktree(task, &mut *agents.reviewer.borrow_mut())?;
    let approval = request_worktree_approval(task)?;
    Ok(OrchestrationResult {
        state: OrchestrationState::AwaitingApproval,
        task_id: task.brief.task_id.clone(),
        worker: Some(worker),
        reviewer: Some(reviewer),
        approval_request: Some(approval),
    })
}

/// Inspect durable records without dispatch, mutation, or implicit recovery.
///
/// # Errors
/// Task preparation, injected agent execution, review, or state persistence fails.
pub fn inspect_offline_task(
    task: &TaskWorkspace,
) -> Result<OrchestrationResult, WorktreeDelegationError> {
    let task_id = task.brief.task_id.as_str();
    let recovery = reconcile(task)?;
    if recovery != RecoveryState::WorkerRecorded {
        return Ok(OrchestrationResult::bare(recovery.into(), task_id));
    }
    let directory = task.state_root.join(task_id);
    let fence = directory.join("review_dispatch.json");
    if fence.is_symlink_lexical() {
        return Ok(OrchestrationResult::bare(
            OrchestrationState::ManualIntervention,
            task_id,
        ));
    }
    let review_fenced = fence.exists();
    let Ok(current_phase) = phase(&task.state_root, &task.brief) else {
        return Ok(OrchestrationResult::bare(
            OrchestrationState::ManualIntervention,
            task_id,
        ));
    };
    Ok(match current_phase {
        Phase::AwaitingApproval => OrchestrationResult {
            approval_request: Some(directory.join("approval-request.json")),
            ..OrchestrationResult::bare(OrchestrationState::AwaitingApproval, task_id)
        },
        Phase::Reviewed => OrchestrationResult::bare(OrchestrationState::Reviewed, task_id),
        Phase::Created | Phase::WorkerDone => OrchestrationResult::bare(
            if review_fenced {
                OrchestrationState::ReviewUncertain
            } else {
                OrchestrationState::WorkerRecorded
            },
            task_id,
        ),
    })
}
