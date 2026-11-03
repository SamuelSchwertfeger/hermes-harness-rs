//! Build a deterministic read-only verification contract for an integration worktree.
//!
//! No command execution is delegated here. A separate sandboxed verifier must
//! consume the contract and attach actual execution evidence.

use crate::util::PathLexical as _;
use std::path::Path;

use crate::git::worktrees::{WorktreeError, audit_worktree};
use crate::multiagent::runtime::WorkflowError;
use crate::util::{resolve_lenient, resolve_strict, sha256_hex};

/// Failure while preparing or running verification.
#[derive(Debug, thiserror::Error)]
pub enum VerificationError {
    /// Rule violation (Python `WorkflowError`).
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Ownership or Git boundary violation from the worktree audit.
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl VerificationError {
    pub(crate) fn msg(message: &str) -> Self {
        Self::Workflow(WorkflowError::msg(message))
    }
}

/// Snapshot of the audited changes a verifier must bind its evidence to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VerificationManifest {
    /// Resolved baseline commit.
    pub baseline: String,
    /// Commit hash of `HEAD`.
    pub head: String,
    /// Sorted changed paths.
    pub changed_paths: Vec<String>,
    /// SHA-256 per changed path; `None` for deleted files.
    pub file_digests: Vec<(String, Option<String>)>,
    /// Never set here: tests are executed elsewhere.
    pub verified_by_tests: bool,
}

/// Reaudit and fingerprint changed files without running generated code.
///
/// # Errors
/// Returns [`VerificationError`] for a non-worktree, a failed audit, an unsafe or non-file
/// changed path, or a read failure.
pub fn prepare_verification(
    workspace: &Path,
    baseline: &str,
    owned_paths: &[String],
) -> Result<VerificationManifest, VerificationError> {
    if workspace.is_symlink_lexical() || !workspace.is_dir() {
        return Err(VerificationError::msg(
            "Verification requires a real worktree",
        ));
    }
    let root = resolve_strict(workspace)?;
    let audit = audit_worktree(&root, baseline, owned_paths)?;
    let mut changes = Vec::with_capacity(audit.changed_paths.len());
    for path in &audit.changed_paths {
        let candidate = root.join(path);
        if candidate.is_symlink_lexical() || !resolve_lenient(&candidate).starts_with(&root) {
            return Err(VerificationError::msg(
                "Unsafe changed path during verification",
            ));
        }
        let digest = if candidate.is_file() {
            Some(sha256_hex(&std::fs::read(&candidate)?))
        } else if !candidate.exists() {
            None
        } else {
            return Err(VerificationError::msg("Non-file in changed paths"));
        };
        changes.push((path.clone(), digest));
    }
    Ok(VerificationManifest {
        baseline: audit.baseline,
        head: audit.head,
        changed_paths: audit.changed_paths,
        file_digests: changes,
        verified_by_tests: false,
    })
}
