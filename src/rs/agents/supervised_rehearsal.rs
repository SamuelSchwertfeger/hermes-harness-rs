//! Run the approved-workspace workflow with offline doubles only.
//!
//! This is a rehearsal seam, NOT a Hermes agent adapter. It does not call a gateway,
//! launch a process, submit a prompt, or turn a disabled invocation executable.

use std::path::{Path, PathBuf};

use super::supervised_agents::{
    MAX_SUPERVISED_DEADLINE_SECONDS, MAX_SUPERVISED_TURNS, SupervisedAgentsError, SupervisedPlan,
    SupervisedPolicy, prepare_supervised_plan,
};
use crate::core::delegation::DelegationError;
use crate::git::worktree_delegation::{
    TaskWorkspace, WorktreeDelegationError, delegate_in_worktree, prepare_worktree,
    request_worktree_approval, review_worktree, verified_audit,
};
use crate::supervised::offline_orchestrator::{
    OfflineAgents, OrchestrationResult, OrchestrationState,
};

/// Rehearsal refused or an underlying step failed.
#[derive(Debug, thiserror::Error)]
pub enum RehearsalError {
    /// Policy does not authorize the task (Python `PermissionError`).
    #[error("{0}")]
    Permission(String),
    /// Budgets outside supervised limits (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// Delegation workflow failure.
    #[error(transparent)]
    Worktree(#[from] WorktreeDelegationError),
    /// Supervised plan failure.
    #[error(transparent)]
    Plan(#[from] SupervisedAgentsError),
}

/// Offline task outcome plus the explicitly non-executable role plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RehearsalResult {
    /// Orchestration result.
    pub task: OrchestrationResult,
    /// Disabled role plan.
    pub plan: SupervisedPlan,
}

/// Source repository and authorization for one supervised rehearsal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RehearsalContext {
    /// Source repository.
    pub source_repo: PathBuf,
    /// Supervised authorization.
    pub policy: SupervisedPolicy,
}

impl RehearsalContext {
    /// Bundle a source repository and policy.
    #[must_use]
    pub fn new(source_repo: &Path, policy: SupervisedPolicy) -> Self {
        Self {
            source_repo: source_repo.to_path_buf(),
            policy,
        }
    }
}

fn check_request(
    task: &TaskWorkspace,
    agents: &OfflineAgents<'_>,
    policy: &SupervisedPolicy,
) -> Result<(), RehearsalError> {
    if policy.authorized_task_id != task.brief.task_id {
        return Err(RehearsalError::Permission(
            "Policy does not authorize this task".to_owned(),
        ));
    }
    if policy.authorized_workspace != task.workspace || !task.workspace.is_absolute() {
        return Err(RehearsalError::Permission(
            "Policy must bind the exact absolute task workspace".to_owned(),
        ));
    }
    if agents.same_agent() || task.brief.worker_id == task.brief.reviewer_id {
        return Err(WorktreeDelegationError::from(DelegationError::Message(
            "Rehearsal requires distinct worker and reviewer implementations".to_owned(),
        ))
        .into());
    }
    // Fail before creating a reservation for invalid budgets.
    let deadline_ok = (1..=MAX_SUPERVISED_DEADLINE_SECONDS).contains(&policy.deadline_seconds);
    let turns_ok = (1..=MAX_SUPERVISED_TURNS).contains(&policy.max_turns);
    if !deadline_ok || !turns_ok {
        return Err(RehearsalError::Value(
            "Rehearsal budgets outside supervised limits".to_owned(),
        ));
    }
    Ok(())
}

/// One-shot rehearsal; preserves evidence and never resumes uncertain work.
///
/// Authorization is checked before reservation. Worktree preparation happens
/// before preflight because preflight requires the worktree to exist. No dispatch
/// occurs unless its exact workspace passes the regular supervised preflight.
///
/// # Errors
/// Task preparation, scripted execution, or worktree cleanup fails.
pub fn run_supervised_rehearsal(
    task: &TaskWorkspace,
    agents: &OfflineAgents<'_>,
    context: &RehearsalContext,
) -> Result<RehearsalResult, RehearsalError> {
    let policy = &context.policy;
    check_request(task, agents, policy)?;

    prepare_worktree(task, &context.source_repo)?;
    let plan = prepare_supervised_plan(&task.brief, policy)?;
    let worker = delegate_in_worktree(task, &mut *agents.worker.borrow_mut())?;
    verified_audit(task)?;
    let reviewer = review_worktree(task, &mut *agents.reviewer.borrow_mut())?;
    let approval = request_worktree_approval(task)?;

    Ok(RehearsalResult {
        task: OrchestrationResult {
            state: OrchestrationState::AwaitingApproval,
            task_id: task.brief.task_id.clone(),
            worker: Some(worker),
            reviewer: Some(reviewer),
            approval_request: Some(approval),
        },
        plan,
    })
}
