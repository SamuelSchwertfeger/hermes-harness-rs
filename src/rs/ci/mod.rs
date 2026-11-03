//! Offline CI requests, GitHub Actions observations and the read-only CI gate.
pub mod ci_gate;
pub mod ci_observation;
pub mod ci_request;
pub mod github_ci_reader;

use crate::core::delegation::DelegationError;

/// Errors of CI requests, observations, and GitHub assessment.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CiError {
    /// Rule violation.
    #[error("{0}")]
    Message(String),
    /// GitHub transport failure (requests never include credentials in URLs).
    #[error("GitHub API read failed; CI state is unknown")]
    GitHubTransport(#[source] Box<github_ci_reader::GitHubTransportError>),
    /// GitHub response read failure.
    #[error("GitHub API read failed; CI state is unknown")]
    GitHubRead(#[source] std::io::Error),
    /// GitHub response JSON failure.
    #[error("GitHub API returned malformed JSON")]
    GitHubJson(#[source] serde_json::Error),
    /// Core delegation failure.
    #[error(transparent)]
    Delegation(#[from] DelegationError),
    /// Git or worktree ownership failure.
    #[error(transparent)]
    Worktree(#[from] crate::git::worktrees::WorktreeError),
    /// Worktree-backed delegation failure.
    #[error(transparent)]
    WorktreeDelegation(#[from] crate::git::worktree_delegation::WorktreeDelegationError),
    /// Reservation, recovery, or task-lock failure.
    #[error(transparent)]
    Reservation(#[from] crate::core::task_reservation::ReservationError),
}
