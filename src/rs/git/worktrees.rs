//! Bounded Git worktree lifecycle and path-ownership audit.
//!
//! This is a Git filesystem boundary, not an OS security sandbox. Untrusted
//! agents must run in an independently restricted container or account.

use crate::util::PathLexical as _;
use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const GIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Ticket identity, branch and owned paths of one worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSpec {
    /// Ticket identifier (`[a-z0-9][a-z0-9-]{0,63}`).
    pub ticket_id: String,
    /// Branch name; must equal `harness/<ticket-id>`.
    pub branch: String,
    /// Relative paths the worktree may change.
    pub owned_paths: Vec<String>,
}

/// Result of a successful ownership audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeAudit {
    /// Sorted changed paths relative to the worktree root.
    pub changed_paths: Vec<String>,
    /// Commit hash of `HEAD`.
    pub head: String,
    /// Resolved baseline commit hash.
    pub baseline: String,
}

/// Git or ownership boundary violation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorktreeError {
    /// Invalid ticket, branch, or owned path specification.
    #[error("{0}")]
    InvalidInput(String),
    /// Git execution or output failure.
    #[error("{0}")]
    Git(String),
    /// Invalid repository or workspace location.
    #[error("{0}")]
    Workspace(String),
    /// Repository or worktree has uncommitted changes.
    #[error("{0}")]
    Dirty(String),
    /// Changed paths violate worktree ownership boundaries.
    #[error("{0}")]
    Ownership(String),
    /// Worktree branch does not match the expected branch.
    #[error("{0}")]
    UnexpectedBranch(String),
    /// Filesystem failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub use crate::util::{resolve_lenient, resolve_strict};

fn git_failed(args: &[&str]) -> WorktreeError {
    let name = args.first().copied().unwrap_or_default();
    WorktreeError::Git(format!("Git operation failed: {name}"))
}

/// Run Git with a scrubbed environment and a 15 second timeout.
///
/// # Errors
/// The worktree specification or changed paths violate policy, or Git or filesystem operations
/// fail.
pub fn git(repo: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .env("HOME", repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", root);
    }
    let mut child = command.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            WorktreeError::Git("Git executable not found".to_owned())
        } else {
            git_failed(args)
        }
    })?;
    let mut stdout = child.stdout.take().ok_or_else(|| git_failed(args))?;
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < GIT_TIMEOUT => thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(git_failed(args));
            }
        }
    };
    let raw = reader.join().map_err(|_| git_failed(args))?;
    if !status.success() {
        return Err(git_failed(args));
    }
    let text = String::from_utf8(raw)
        .map_err(|_| WorktreeError::Git("Git output is not valid UTF-8".to_owned()))?;
    if args.contains(&"-z") {
        Ok(text)
    } else {
        Ok(crate::util::py_strip(&text).to_owned())
    }
}

/// Reject empty, absolute, dotted or backslash/NUL-containing relative paths.
///
/// # Errors
/// The path is absolute, contains traversal, or refers to a forbidden location.
fn validate_relative(path: &str) -> Result<(), WorktreeError> {
    let invalid_components = path.split('/').any(|part| part == ".." || part == ".");
    let invalid_chars = path.contains('\\') || path.contains('\0');
    let invalid_path = path.is_empty() || path.starts_with('/');
    if invalid_path || invalid_components || invalid_chars {
        return Err(WorktreeError::InvalidInput(format!(
            "Invalid relative path: {path:?}"
        )));
    }
    Ok(())
}

fn is_ticket_id(value: &str) -> bool {
    let mut chars = value.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok
        && value.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Validate ticket ID, branch name and owned paths of a spec.
///
/// # Errors
/// Worktree task identity, branch, base reference, or owned paths violate policy.
fn validate_spec(spec: &WorktreeSpec) -> Result<(), WorktreeError> {
    if !is_ticket_id(&spec.ticket_id) {
        return Err(WorktreeError::InvalidInput("Invalid ticket ID".to_owned()));
    }
    if !spec.branch.starts_with("harness/") || spec.branch != format!("harness/{}", spec.ticket_id)
    {
        return Err(WorktreeError::InvalidInput(
            "Branch must match harness/<ticket-id>".to_owned(),
        ));
    }
    if spec.owned_paths.is_empty() {
        return Err(WorktreeError::InvalidInput(
            "At least one owned path is required".to_owned(),
        ));
    }
    spec.owned_paths
        .iter()
        .try_for_each(|path| validate_relative(path))
}

fn same_path(text: &str, path: &Path) -> bool {
    Path::new(text) == path
}

/// Create a worktree at a new path; never remove or overwrite existing data.
///
/// # Errors
/// The worktree specification or changed paths violate policy, or Git or filesystem operations
/// fail.
pub fn create_worktree(
    repo: &Path,
    workspace: &Path,
    spec: &WorktreeSpec,
) -> Result<PathBuf, WorktreeError> {
    validate_spec(spec)?;
    let repo = resolve_strict(repo)?;
    let workspace = resolve_lenient(workspace);
    if workspace.starts_with(&repo) || repo.starts_with(&workspace) {
        return Err(WorktreeError::Workspace(
            "Workspace must be disjoint from repository".to_owned(),
        ));
    }
    if workspace.exists() || workspace.is_symlink_lexical() {
        return Err(WorktreeError::Workspace(
            "Workspace already exists".to_owned(),
        ));
    }
    if !same_path(&git(&repo, &["rev-parse", "--show-toplevel"])?, &repo) {
        return Err(WorktreeError::Workspace(
            "Repository path must be the Git root".to_owned(),
        ));
    }
    if !git(&repo, &["status", "--porcelain"])?.is_empty() {
        return Err(WorktreeError::Dirty(
            "Refusing to create worktree from dirty source repository".to_owned(),
        ));
    }
    let target = crate::util::path_str(&workspace)?;
    git(
        &repo,
        &["worktree", "add", "-b", &spec.branch, target, "HEAD"],
    )?;
    Ok(workspace)
}

/// Split Git NUL-terminated paths, dropping empty entries and keeping whitespace within names.
#[doc(hidden)]
pub fn split_z_paths(raw: &str) -> impl Iterator<Item = &str> {
    raw.split('\0').filter(|name| !name.is_empty())
}

/// Track committed, staged, unstaged and untracked paths, including deletions.
///
/// # Errors
/// The worktree specification or changed paths violate policy, or Git or filesystem operations
/// fail.
fn changed_paths(repo: &Path, baseline: &str) -> Result<Vec<String>, WorktreeError> {
    let command_sets: [&[&str]; 4] = [
        &["diff", "--name-only", "-z", baseline, "HEAD"],
        &["diff", "--name-only", "-z"],
        &["diff", "--cached", "--name-only", "-z"],
        &["ls-files", "--others", "--exclude-standard", "-z"],
    ];
    let mut names = BTreeSet::new();
    for command in command_sets {
        let raw = git(repo, command)?;
        names.extend(split_z_paths(&raw).map(str::to_owned));
    }
    Ok(names.into_iter().collect())
}

fn check_changed(root: &Path, name: &str, owned_paths: &[String]) -> Result<(), WorktreeError> {
    validate_relative(name)?;
    let owned = owned_paths.iter().any(|own| {
        name == own
            || name
                .strip_prefix(own.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    });
    if !owned {
        return Err(WorktreeError::Ownership(format!(
            "Path outside ownership: {name}"
        )));
    }
    let candidate = root.join(name);
    if !resolve_lenient(&candidate).starts_with(root) {
        return Err(WorktreeError::Ownership(format!(
            "Path escapes worktree: {name}"
        )));
    }
    let mut current = candidate.as_path();
    while current != root {
        if current.is_symlink_lexical() {
            return Err(WorktreeError::Ownership(format!("Symlink touched: {name}")));
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    Ok(())
}

/// Fail closed on ownership violations and symlinks in changed path ancestry.
///
/// # Errors
/// The worktree specification or changed paths violate policy, or Git or filesystem operations
/// fail.
pub fn audit_worktree(
    worktree: &Path,
    baseline: &str,
    owned_paths: &[String],
) -> Result<WorktreeAudit, WorktreeError> {
    let root = resolve_strict(worktree)?;
    if owned_paths.is_empty() {
        return Err(WorktreeError::InvalidInput(
            "Missing owned paths".to_owned(),
        ));
    }
    owned_paths
        .iter()
        .try_for_each(|owned| validate_relative(owned))?;
    let revision = format!("{baseline}^{{commit}}");
    let base = git(
        &root,
        &["rev-parse", "--verify", "--end-of-options", &revision],
    )?;
    let head = git(&root, &["rev-parse", "HEAD"])?;
    let changed = changed_paths(&root, &base)?;
    changed
        .iter()
        .try_for_each(|name| check_changed(&root, name, owned_paths))?;
    Ok(WorktreeAudit {
        changed_paths: changed,
        head,
        baseline: base,
    })
}

/// Remove only an explicitly named clean worktree; retain its branch for audit.
///
/// # Errors
/// The worktree specification or changed paths violate policy, or Git or filesystem operations
/// fail.
pub fn remove_worktree(
    repo: &Path,
    worktree: &Path,
    expected_branch: &str,
) -> Result<(), WorktreeError> {
    let root = resolve_strict(worktree)?;
    let branch = git(&root, &["symbolic-ref", "--short", "HEAD"])?;
    if branch != expected_branch || !branch.starts_with("harness/") {
        return Err(WorktreeError::UnexpectedBranch(
            "Refusing removal of unexpected worktree branch".to_owned(),
        ));
    }
    if !git(&root, &["status", "--porcelain"])?.is_empty() {
        return Err(WorktreeError::Dirty(
            "Refusing removal of dirty worktree".to_owned(),
        ));
    }
    git(repo, &["worktree", "remove", crate::util::path_str(&root)?])?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn untracked_path(name: &str) {
        let tmp = tempfile::tempdir().unwrap();
        let root = resolve_strict(tmp.path()).unwrap();
        git(&root, &["init"]).unwrap();
        git(&root, &["config", "user.name", "Harness Test"]).unwrap();
        git(&root, &["config", "user.email", "tests@example.invalid"]).unwrap();
        git(&root, &["commit", "--allow-empty", "-m", "Initial"]).unwrap();
        let baseline = git(&root, &["rev-parse", "HEAD"]).unwrap();
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "untracked").unwrap();
        let raw = git(&root, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap();
        assert_eq!(raw, format!("{name}\0"));
        assert_eq!(changed_paths(&root, &baseline).unwrap(), vec![name]);
        assert!(matches!(
            audit_worktree(&root, &baseline, &["src/x".to_owned()]),
            Err(WorktreeError::Ownership(_))
        ));
    }

    #[test]
    fn leading_space_is_preserved_and_unowned() {
        untracked_path(" src/x");
    }

    #[cfg(unix)]
    #[test]
    fn trailing_newline_is_preserved_and_unowned() {
        untracked_path("src/x\n");
    }
}
