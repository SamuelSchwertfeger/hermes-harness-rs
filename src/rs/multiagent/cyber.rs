//! Explicit authorization boundary for optional Cyber Warrior worker roles.
//!
//! This gate does not grant process isolation or enable any live model.
//! Security-role execution requires an operator-bound repository, task and path grant.

use crate::util::PathLexical as _;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::util::{is_py_space, resolve_lenient};

/// Missing or excessively broad operator authorization.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CyberAuthorizationError(pub String);

/// Operator-authorized scope for a local security specialist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CyberGrant {
    /// Repository the grant is bound to.
    pub repository: PathBuf,
    /// Node IDs the operator authorized.
    pub task_ids: Vec<String>,
    /// Owned paths the operator authorized.
    pub owned_paths: Vec<String>,
    /// Operator identity.
    pub approved_by: String,
}

impl CyberGrant {
    /// Build a grant from borrowed parts.
    #[must_use]
    pub fn new(
        repository: impl Into<PathBuf>,
        task_ids: &[&str],
        owned_paths: &[&str],
        approved_by: &str,
    ) -> Self {
        Self {
            repository: repository.into(),
            task_ids: task_ids.iter().map(|item| (*item).to_owned()).collect(),
            owned_paths: owned_paths.iter().map(|item| (*item).to_owned()).collect(),
            approved_by: approved_by.to_owned(),
        }
    }
}

fn deny<T>(message: &str) -> Result<T, CyberAuthorizationError> {
    Err(CyberAuthorizationError(message.to_owned()))
}

/// Reject unapproved Cyber Warrior dispatch before creating task state.
///
/// # Errors
/// [`CyberAuthorizationError`] for a missing grant, invalid identity or repository, repository
/// mismatch, unauthorized task, or ownership beyond the grant.
pub fn validate_cyber_node(
    node_id: &str,
    owned_paths: &[String],
    repository: &Path,
    grant: Option<&CyberGrant>,
) -> Result<(), CyberAuthorizationError> {
    let Some(grant) = grant else {
        return deny("Cyber Warrior requires an explicit operator grant");
    };
    if grant.approved_by.chars().all(is_py_space) || grant.repository.is_symlink_lexical() {
        return deny("Invalid Cyber Warrior grant identity or repository");
    }
    if resolve_lenient(repository) != resolve_lenient(&grant.repository) {
        return deny("Cyber Warrior grant repository mismatch");
    }
    if !grant.task_ids.iter().any(|item| item == node_id) {
        return deny("Cyber Warrior task not authorized");
    }
    let allowed: BTreeSet<&str> = grant.owned_paths.iter().map(String::as_str).collect();
    if !owned_paths
        .iter()
        .all(|path| allowed.contains(path.as_str()))
    {
        return deny("Cyber Warrior ownership exceeds grant");
    }
    Ok(())
}
