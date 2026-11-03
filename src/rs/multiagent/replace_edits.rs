//! Content-addressed file replacements for trusted disposable Git worktrees.
//!
//! Prepared replacements must be checked against exact previous content hashes.
//! This module does not execute model code or commit changes.

use crate::multiagent::model_edits::{is_lexically_safe_path, is_owned};
use crate::util::PathLexical as _;
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::git::worktrees::{WorktreeAudit, WorktreeError, audit_worktree};
use crate::multiagent::runtime::WorkflowError;
use crate::util::{resolve_lenient, resolve_strict, sha256_hex};

/// Maximum number of replacements per application.
pub const MAX_REPLACEMENTS: usize = 16;
/// Maximum size of one replacement file in bytes.
pub const MAX_REPLACEMENT_BYTES: usize = 48 * 1024;

/// One exact-content replacement of an existing file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReplacement {
    /// Relative path inside the worktree.
    pub path: String,
    /// SHA-256 hex digest the current file content must have.
    pub expected_sha256: String,
    /// New file content.
    pub content: String,
}

/// Failure while applying replacements (Python `WorkflowError` or `WorktreeError`).
#[derive(Debug, thiserror::Error)]
pub enum ReplaceEditsError {
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

fn fail<T>(message: &str) -> Result<T, ReplaceEditsError> {
    Err(WorkflowError::msg(message).into())
}

/// `O_NOFOLLOW` as an `i32` open flag (Python `getattr(os, 'O_NOFOLLOW', 0)`).
#[cfg(unix)]
pub(crate) fn nofollow_flag() -> i32 {
    i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or(0)
}

fn replacement_path(name: &str, owned_paths: &[String], seen: &BTreeSet<String>) -> bool {
    is_lexically_safe_path(name) && !seen.contains(name) && is_owned(name, owned_paths)
}

fn prepare_replacement<'a>(
    root: &Path,
    item: &'a FileReplacement,
    owned_paths: &[String],
    seen: &mut BTreeSet<String>,
) -> Result<(PathBuf, &'a [u8]), ReplaceEditsError> {
    let name = item.path.as_str();
    if !replacement_path(name, owned_paths, seen) {
        return fail("Unsafe, unowned, or duplicate replacement path");
    }
    seen.insert(name.to_owned());
    let target = root.join(name);
    if !resolve_lenient(&target).starts_with(root) {
        return fail("Replacement path escapes worktree");
    }
    let mut current = target.as_path();
    while current != root {
        if current.is_symlink_lexical() {
            return fail("Replacement path traverses a symlink");
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    if !target.is_file() {
        return fail("Replacement target must be an existing regular file");
    }
    let original = std::fs::read(&target)?;
    if sha256_hex(&original) != item.expected_sha256 {
        return fail("Replacement preimage digest mismatch");
    }
    let data = item.content.as_bytes();
    if data.len() > MAX_REPLACEMENT_BYTES {
        return fail("Replacement file exceeds size bound");
    }
    Ok((target, data))
}

fn write_replacements(targets: &[(PathBuf, &[u8])]) -> std::io::Result<()> {
    for (target, data) in targets {
        let mut options = OpenOptions::new();
        options.write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(nofollow_flag());
        }
        let mut stream = options.open(target)?;
        stream.write_all(data)?;
        stream.flush()?;
        stream.sync_all()?;
    }
    Ok(())
}

/// Apply reviewed exact-content replacements, rejecting stale state.
///
/// A failed or interrupted write retains evidence, never silently retries.
/// Caller must serialize filesystem changes and supply a disposable checkout.
///
/// # Errors
///
/// Returns [`ReplaceEditsError`] when authorization is missing, the replacement count or
/// workspace is invalid, the worktree is not at its clean baseline, a replacement path is
/// unsafe, unowned, duplicate or symlinked, a preimage digest is stale, content is oversized,
/// the audit differs from the intended paths, or the filesystem or Git fails.
pub fn apply_replacements(
    workspace: &Path,
    replacements: &[FileReplacement],
    owned_paths: &[String],
    baseline: &str,
    authorized: bool,
) -> Result<WorktreeAudit, ReplaceEditsError> {
    if !authorized {
        return fail("Explicit replacement authorization required");
    }
    if replacements.is_empty() || replacements.len() > MAX_REPLACEMENTS {
        return fail("Invalid replacement count");
    }
    if workspace.is_symlink_lexical() || !workspace.is_dir() {
        return fail("Expected real disposable worktree");
    }
    let root = resolve_strict(workspace)?;
    let before = audit_worktree(&root, baseline, owned_paths)?;
    if before.head != baseline || !before.changed_paths.is_empty() {
        return fail("Replacement requires unchanged baseline worktree");
    }
    let mut seen = BTreeSet::new();
    let targets = replacements
        .iter()
        .map(|item| prepare_replacement(&root, item, owned_paths, &mut seen))
        .collect::<Result<Vec<_>, _>>()?;
    write_replacements(&targets)?;
    let after = audit_worktree(&root, baseline, owned_paths)?;
    let changed: BTreeSet<String> = after.changed_paths.iter().cloned().collect();
    if changed != seen {
        return fail("Replacement audit differs from intended changed paths");
    }
    Ok(after)
}
