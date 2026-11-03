//! Preflight-only bridge from delegated tasks to disabled Hermes invocations.
//!
//! Plans never invoke Hermes, providers, shells, or gateway sessions. Admission
//! requires separately verified runtime isolation and cancellation in future work.

use crate::util::PathLexical as _;
use std::path::PathBuf;

use crate::core::delegation::{Brief, DelegationError};
use crate::gateway::hermes_adapter::{
    HermesAdapterError, PreparedInvocation, TaskKind, TaskRequest, execute, prepare,
};

/// Longest deadline a supervised preflight may request.
pub const MAX_SUPERVISED_DEADLINE_SECONDS: i64 = 300;
/// Most turns a supervised preflight may request.
pub const MAX_SUPERVISED_TURNS: i64 = 5;

/// Errors raised while planning supervised roles.
#[derive(Debug, thiserror::Error)]
pub enum SupervisedAgentsError {
    /// Missing task authorization (Python `PermissionError`).
    #[error("{0}")]
    Permission(String),
    /// Invalid policy or workspace (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// Role identity violation.
    #[error(transparent)]
    Delegation(#[from] DelegationError),
    /// Hermes adapter refusal.
    #[error(transparent)]
    Adapter(#[from] HermesAdapterError),
}

/// Explicit task authorization and narrow resource budgets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisedPolicy {
    /// The only task this policy authorizes.
    pub authorized_task_id: String,
    /// The only workspace this policy authorizes.
    pub authorized_workspace: PathBuf,
    /// Deadline in seconds.
    pub deadline_seconds: i64,
    /// Turn budget.
    pub max_turns: i64,
}

impl SupervisedPolicy {
    /// Build a policy with the default budgets (60 seconds, 2 turns).
    #[must_use]
    pub fn new(authorized_task_id: &str, authorized_workspace: impl Into<PathBuf>) -> Self {
        Self {
            authorized_task_id: authorized_task_id.to_owned(),
            authorized_workspace: authorized_workspace.into(),
            deadline_seconds: 60,
            max_turns: 2,
        }
    }
}

/// Separate disabled invocations for distinct worker and reviewer roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisedPlan {
    /// Task identity.
    pub task_id: String,
    /// Worker identity.
    pub worker_id: String,
    /// Reviewer identity.
    pub reviewer_id: String,
    /// Worker invocation.
    pub worker: PreparedInvocation,
    /// Reviewer invocation.
    pub reviewer: PreparedInvocation,
}

impl SupervisedPlan {
    /// Fail closed: manifests alone never establish execution eligibility.
    ///
    /// # Errors
    /// Always refuses execution because a plan alone does not establish runtime qualification.
    pub fn require_executable(&self) -> Result<(), SupervisedAgentsError> {
        execute(&self.worker)?;
        execute(&self.reviewer)?;
        Err(HermesAdapterError::Safety(
            "Supervised launch requires separately qualified sandbox and gateway admission"
                .to_owned(),
        )
        .into())
    }
}

fn prompt(role: &str, identity: &str, brief: &Brief, closing: &str) -> String {
    format!(
        "{role} identity: {identity}\nObjective: {}\nOwned paths: {}\n{closing}",
        brief.objective,
        brief.owned_paths.join(", ")
    )
}

fn check_authorization(
    brief: &Brief,
    policy: &SupervisedPolicy,
) -> Result<(), SupervisedAgentsError> {
    if brief.task_id.is_empty() || policy.authorized_task_id != brief.task_id {
        return Err(SupervisedAgentsError::Permission(
            "Task-specific authorization is required".to_owned(),
        ));
    }
    if brief.worker_id.is_empty()
        || brief.reviewer_id.is_empty()
        || brief.worker_id == brief.reviewer_id
    {
        return Err(DelegationError::Message(
            "Worker and reviewer identities must differ".to_owned(),
        )
        .into());
    }
    let workspace = &policy.authorized_workspace;
    if workspace.is_symlink_lexical() || !workspace.is_absolute() || !workspace.is_dir() {
        return Err(SupervisedAgentsError::Value(
            "Authorized workspace must be an existing, absolute real directory".to_owned(),
        ));
    }
    if policy.deadline_seconds > MAX_SUPERVISED_DEADLINE_SECONDS
        || policy.max_turns > MAX_SUPERVISED_TURNS
    {
        return Err(SupervisedAgentsError::Value(
            "Supervised budgets exceed preflight limits".to_owned(),
        ));
    }
    Ok(())
}

/// Translate one task into distinct disabled Hermes role requests.
///
/// The preflight deliberately performs no reservation, dispatch, or writes.
/// The reviewer remains a separate invocation, never a reused worker session.
///
/// # Errors
/// The policy does not authorize execution or the task workspace violates the policy.
pub fn prepare_supervised_plan(
    brief: &Brief,
    policy: &SupervisedPolicy,
) -> Result<SupervisedPlan, SupervisedAgentsError> {
    check_authorization(brief, policy)?;
    let worker_prompt = prompt(
        "Worker",
        &brief.worker_id,
        brief,
        "Make no changes outside owned paths. Do not push, merge, or grant approval.",
    );
    let review_prompt = prompt(
        "Reviewer",
        &brief.reviewer_id,
        brief,
        "Independently review recorded worker evidence after audit. \
         Do not modify files, push, merge, or grant approval.",
    );
    let request = |suffix: &str, kind: TaskKind, text: &str| {
        let mut request = TaskRequest::new(
            &format!("{}-{suffix}", brief.task_id),
            kind,
            text,
            &policy.authorized_workspace,
        );
        request.deadline_seconds = policy.deadline_seconds;
        request.max_turns = policy.max_turns;
        request
    };
    let worker = prepare(&request("worker", TaskKind::Plan, &worker_prompt))?;
    let reviewer = prepare(&request("reviewer", TaskKind::Review, &review_prompt))?;
    Ok(SupervisedPlan {
        task_id: brief.task_id.clone(),
        worker_id: brief.worker_id.clone(),
        reviewer_id: brief.reviewer_id.clone(),
        worker,
        reviewer,
    })
}
