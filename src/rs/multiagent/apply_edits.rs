//! Explicit, restricted application of admitted *new-file* proposals.
//!
//! This is a filesystem mutation boundary for trusted, already-created disposable
//! worktrees. It is NOT a sandbox and never executes generated source or Git commits.

use crate::multiagent::model_edits::is_owned;
use crate::util::PathLexical as _;
use std::collections::BTreeSet;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::git::worktrees::{WorktreeAudit, WorktreeError, audit_worktree};
use crate::multiagent::model_edits::{EditProposal, ProposedFile};
use crate::multiagent::runtime::WorkflowError;
use crate::util::{resolve_lenient, resolve_strict};

/// Audit of an explicitly authorized, successful proposal application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedProposal {
    /// Task the proposal belonged to.
    pub task_id: String,
    /// Created paths in proposal order.
    pub paths: Vec<String>,
    /// Worktree audit after the application.
    pub audit: WorktreeAudit,
}

/// Failure while applying a proposal (Python `WorkflowError` or `WorktreeError`).
#[derive(Debug, thiserror::Error)]
pub enum ApplyEditsError {
    /// Workflow rule violation.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Git or ownership failure from the worktree audit.
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    /// Filesystem failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn fail<T>(message: &str) -> Result<T, ApplyEditsError> {
    Err(WorkflowError::msg(message).into())
}

/// Refuse symlink traversal and any path escaping the given worktree.
fn target(root: &Path, relative: &Path) -> Result<PathBuf, ApplyEditsError> {
    let target = root.join(relative);
    if !resolve_lenient(&target).starts_with(root) {
        return fail("Proposal path escapes worktree");
    }
    let mut current = target.as_path();
    while current != root {
        if current.is_symlink_lexical() {
            return fail("Proposal path traverses a symlink");
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    Ok(target)
}

fn prepare_new_file<'a>(
    root: &Path,
    file: &'a ProposedFile,
    owned_paths: &[String],
) -> Result<(PathBuf, &'a [u8]), ApplyEditsError> {
    let path = file.path.as_str();
    let owned = is_owned(path, owned_paths);
    if !owned {
        return fail("Proposal path exceeds assigned ownership");
    }
    let resolved = target(root, Path::new(path))?;
    if resolved.exists() || resolved.is_symlink_lexical() {
        return fail("Proposal may only create new files");
    }
    Ok((resolved, file.content.as_bytes()))
}

/// `Path.mkdir(mode=0o700, parents=True, exist_ok=True)`: only the leaf gets the private mode.
fn mkdir_private(dir: &Path) -> std::io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match builder.create(dir) {
                Err(again) if again.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {
                    Ok(())
                }
                other => other,
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        other => other,
    }
}

fn write_new_file(path: &Path, payload: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(super::replace_edits::nofollow_flag());
    }
    let mut handle = options.open(path)?;
    handle.write_all(payload)?;
    handle.flush()?;
    handle.sync_all()
}

fn write_new_files(root: &Path, targets: &[(PathBuf, &[u8])]) -> Result<(), ApplyEditsError> {
    for (path, payload) in targets {
        if let Some(parent) = path.parent() {
            mkdir_private(parent)?;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            return fail("Proposal path escapes worktree");
        };
        target(root, relative)?;
        write_new_file(path, payload)?;
    }
    Ok(())
}

/// Write only absent regular files and re-audit changed paths.
///
/// Caller must supply a disposable worktree. Failed writes leave evidence
/// in place for manual inspection; automatic replay is prohibited.
///
/// # Errors
///
/// Returns [`ApplyEditsError`] when authorization is missing, the workspace is not a real
/// directory, the worktree is not clean at the baseline, a path is unowned, escapes the
/// worktree, traverses a symlink or already exists, the post-write audit differs from the
/// approved proposal, or the filesystem or Git fails.
pub fn apply_new_files(
    workspace: &Path,
    proposal: &EditProposal,
    owned_paths: &[String],
    baseline: &str,
    authorized: bool,
) -> Result<AppliedProposal, ApplyEditsError> {
    if !authorized {
        return fail("Explicit proposal application authorization required");
    }
    if workspace.is_symlink_lexical() || !workspace.is_dir() {
        return fail("Expected existing real disposable worktree");
    }
    let root = resolve_strict(workspace)?;
    let before = audit_worktree(&root, baseline, owned_paths)?;
    if !before.changed_paths.is_empty() || before.head != baseline {
        return fail("Proposal application requires clean worktree at baseline");
    }
    let targets = proposal
        .files
        .iter()
        .map(|file| prepare_new_file(&root, file, owned_paths))
        .collect::<Result<Vec<_>, _>>()?;
    write_new_files(&root, &targets)?;
    let after = audit_worktree(&root, baseline, owned_paths)?;
    let changed: BTreeSet<&str> = after.changed_paths.iter().map(String::as_str).collect();
    let expected: BTreeSet<&str> = proposal
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    if changed != expected {
        return fail("Applied changes differ from approved proposal");
    }
    Ok(AppliedProposal {
        task_id: proposal.task_id.clone(),
        paths: proposal
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect(),
        audit: after,
    })
}
