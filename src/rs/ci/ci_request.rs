//! Offline CI request anchored to an immutable Git commit and tree.
//!
//! A request is not a CI result, attestation, authorization, or approval. It
//! cannot authenticate external CI and never executes tests or remote commands.

use crate::util::PathLexical as _;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::ci::CiError;
use crate::core::delegation::{DelegationError, Phase, phase, write_once_value};
use crate::core::task_reservation::{RecoveryState, Reservation, load, reconcile, task_lock};
use crate::git::worktree_delegation::{TaskWorkspace, snapshot};
use crate::git::worktrees::{WorktreeAudit, audit_worktree, git, resolve_strict};
use crate::util::{canonical_json_value, sha256_hex};

type Res<T> = Result<T, CiError>;

/// Immutable offline CI request for one committed candidate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct CIRequest {
    /// Task identifier.
    pub task_id: String,
    /// Digest of the task brief.
    pub brief_digest: String,
    /// Baseline commit.
    pub baseline: String,
    /// Candidate commit.
    pub commit: String,
    /// Candidate tree.
    pub tree: String,
    /// Reserved branch.
    pub branch: String,
    /// Changed paths of the candidate.
    pub changed_paths: Vec<String>,
    /// Digest of the reviewed content.
    pub content_sha256: String,
}

fn fail<T>(msg: &str) -> Res<T> {
    Err(CiError::Message(msg.to_owned()))
}

fn read(path: &Path) -> Res<Value> {
    if path.is_symlink_lexical() {
        return fail("Symlink evidence is forbidden");
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path).map_err(DelegationError::from)?)
        .map_err(DelegationError::from)?;
    if !value.is_object() {
        return fail("Invalid evidence document");
    }
    Ok(value)
}

/// Require the committed candidate to match the reviewed worker evidence.
fn validate_reviewed_snapshot(
    task: &TaskWorkspace,
    record: &Reservation,
    audit: &WorktreeAudit,
) -> Res<String> {
    let stored = read(
        &task
            .state_root
            .join(&task.brief.task_id)
            .join("worktree-audit.json"),
    )?;
    let mismatch = "Committed content does not match reviewed worker snapshot";
    let Some(old_audit) = stored.get("audit").filter(|value| value.is_object()) else {
        return fail(mismatch);
    };
    let paths_match = old_audit
        .get("changed_paths")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.len() == audit.changed_paths.len()
                && items
                    .iter()
                    .zip(&audit.changed_paths)
                    .all(|(item, path)| item.as_str() == Some(path))
        });
    if !paths_match || old_audit.get("baseline").and_then(Value::as_str) != Some(&record.baseline) {
        return fail(mismatch);
    }
    let digest = snapshot(&task.workspace, audit)?;
    if stored.get("content_sha256").and_then(Value::as_str) != Some(&digest) {
        return fail(mismatch);
    }
    Ok(digest)
}

/// Require the reserved branch and an unmodified, committed working tree.
fn validate_clean_branch(task: &TaskWorkspace, record: &Reservation, commit: &str) -> Res<()> {
    if task.workspace.is_symlink_lexical()
        || resolve_strict(&task.workspace).map_err(DelegationError::from)?
            != std::path::absolute(&task.workspace).map_err(DelegationError::from)?
    {
        return fail("Noncanonical CI worktree");
    }
    let status = git(
        &task.workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    if !status.is_empty() {
        return fail("CI candidate must be committed and clean");
    }
    let branch = git(&task.workspace, &["symbolic-ref", "--short", "HEAD"])?;
    if record.branch != branch || commit == record.baseline {
        return fail("CI candidate must be on reserved branch with new commit");
    }
    Ok(())
}

/// Inspect reviewed committed work, without changing Git or disk state.
fn candidate(task: &TaskWorkspace) -> Res<CIRequest> {
    let record = load(task)?;
    if reconcile(task)? != RecoveryState::WorkerRecorded {
        return fail("Worker dispatch evidence incomplete or uncertain");
    }
    if !matches!(
        phase(&task.state_root, &task.brief)?,
        Phase::Reviewed | Phase::AwaitingApproval
    ) {
        return fail("Independent offline review required");
    }
    let directory = task.state_root.join(&task.brief.task_id);
    if !directory.join("review_dispatch.json").is_file() {
        return fail("Reviewer dispatch fence missing");
    }
    let commit = git(&task.workspace, &["rev-parse", "HEAD"])?;
    validate_clean_branch(task, &record, &commit)?;
    let tree = git(
        &task.workspace,
        &["rev-parse", &format!("HEAD^{{{}}}", "tree")],
    )?;
    let audit = audit_worktree(&task.workspace, &record.baseline, &task.brief.owned_paths)?;
    let digest = validate_reviewed_snapshot(task, &record, &audit)?;
    Ok(CIRequest {
        task_id: record.task_id,
        brief_digest: record.brief_digest,
        baseline: record.baseline,
        commit,
        tree,
        branch: record.branch,
        changed_paths: audit.changed_paths,
        content_sha256: digest,
    })
}

/// Create a non-overwriting request; never launch CI or approve work.
///
/// # Errors
/// Returns a [`CiError`] when the worker or reviewer evidence is incomplete, the candidate is not
/// committed and clean on the reserved branch, the content differs from the reviewed
/// snapshot, or the request file already exists.
pub fn create_ci_request(task: &TaskWorkspace) -> Res<CIRequest> {
    let _guard = task_lock(&task.state_root, &task.brief.task_id)?;
    let result = candidate(task)?;
    let path = task
        .state_root
        .join(&task.brief.task_id)
        .join("ci-request.json");
    let payload = serde_json::to_value(&result).map_err(DelegationError::from)?;
    let digest = sha256_hex(
        canonical_json_value(&payload)
            .map_err(DelegationError::from)?
            .as_bytes(),
    );
    write_once_value(
        &path,
        &serde_json::json!({"candidate": payload, "sha256": digest}),
    )?;
    Ok(result)
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

fn parse_request(value: &Value) -> Option<CIRequest> {
    let paths = value
        .get("changed_paths")?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    Some(CIRequest {
        task_id: text(value, "task_id")?,
        brief_digest: text(value, "brief_digest")?,
        baseline: text(value, "baseline")?,
        commit: text(value, "commit")?,
        tree: text(value, "tree")?,
        branch: text(value, "branch")?,
        changed_paths: paths,
        content_sha256: text(value, "content_sha256")?,
    })
}

/// Recheck saved request against local Git HEAD and reviewed content, read-only.
///
/// # Errors
/// Returns a [`CiError`] when the saved request is malformed, fails its checksum, or no longer
/// matches the local candidate.
pub fn inspect_ci_request(task: &TaskWorkspace) -> Res<CIRequest> {
    let path = task
        .state_root
        .join(&task.brief.task_id)
        .join("ci-request.json");
    let stored = read(&path)?;
    let Some(value) = stored.get("candidate").filter(|v| v.is_object()) else {
        return fail("Malformed CI request");
    };
    if stored.get("sha256").and_then(Value::as_str)
        != Some(&sha256_hex(
            canonical_json_value(value)
                .map_err(DelegationError::from)?
                .as_bytes(),
        ))
    {
        return fail("CI request checksum mismatch");
    }
    let Some(expected) = parse_request(value) else {
        return fail("Malformed CI request contract");
    };
    if candidate(task)? != expected {
        return fail("CI request no longer matches local candidate");
    }
    Ok(expected)
}
