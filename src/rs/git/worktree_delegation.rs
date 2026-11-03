//! Offline delegation with Git worktree evidence; never launches a provider.
//!
//! Worktree auditing is retrospective, not process isolation. Agents are injected
//! callables and MUST remain controlled offline doubles until OS isolation exists.

use crate::util::PathLexical as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::delegation::{
    Brief, DelegationError, Finding, OfflineAgent, Phase, begin_reserved, delegate, phase,
    request_approval, review, write_once_value,
};
use crate::core::task_reservation::{
    RecoveryState, ReservationError, fence, reconcile, reserve, task_lock,
};
use crate::git::worktrees::{
    WorktreeAudit, WorktreeError, WorktreeSpec, audit_worktree, create_worktree,
};
use crate::util::{canonical_json, canonical_json_value, sha256_hex};

/// Immutable locally recorded worktree baseline and ownership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeBinding {
    /// Task identifier.
    pub task_id: String,
    /// Baseline commit hash.
    pub baseline: String,
    /// Branch name.
    pub branch: String,
    /// Relative paths the worker may change.
    pub owned_paths: Vec<String>,
}

/// State root, workspace path and brief of one delegated task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskWorkspace {
    /// Directory holding durable evidence.
    pub state_root: PathBuf,
    /// Worktree path.
    pub workspace: PathBuf,
    /// Task brief.
    pub brief: Brief,
}

/// Errors of worktree-backed delegation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorktreeDelegationError {
    /// Workflow rule violation.
    #[error(transparent)]
    Delegation(#[from] DelegationError),
    /// Git or ownership failure.
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    /// Reservation failure.
    #[error(transparent)]
    Reservation(#[from] ReservationError),
    /// Filesystem failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed JSON evidence.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

type Res<T> = Result<T, WorktreeDelegationError>;

fn fail<T>(msg: &str) -> Res<T> {
    Err(DelegationError::Message(msg.to_owned()).into())
}

fn read(path: &Path) -> Res<Value> {
    if path.is_symlink_lexical() {
        return fail("Symlink evidence is forbidden");
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if !value.is_object() {
        return fail("Invalid evidence document");
    }
    Ok(value)
}

/// Hash audited file contents, including deletions and untracked files.
///
/// # Errors
/// An audited path is a symlink or non-file, or file reading or digest encoding fails.
pub(crate) fn snapshot(workspace: &Path, audit: &WorktreeAudit) -> Res<String> {
    let mut chunks: Vec<(&str, Option<String>)> = Vec::new();
    for name in &audit.changed_paths {
        let candidate = workspace.join(name);
        if candidate.is_symlink_lexical() {
            return fail("Symlink in audited worktree");
        }
        let digest = if candidate.is_file() {
            Some(sha256_hex(&std::fs::read(&candidate)?))
        } else if !candidate.exists() {
            None
        } else {
            return fail("Non-file in audited paths");
        };
        chunks.push((name, digest));
    }
    Ok(sha256_hex(canonical_json(&chunks)?.as_bytes()))
}

fn binding_path(root: &Path, brief: &Brief) -> Res<PathBuf> {
    if root.is_symlink_lexical() || !root.is_dir() {
        return fail("Evidence root must be a real directory");
    }
    Ok(root.join(&brief.task_id).join("worktree-binding.json"))
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn load_binding(root: &Path, brief: &Brief) -> Res<WorktreeBinding> {
    let record = read(&binding_path(root, brief)?)?;
    let Some(value) = record.get("binding").filter(|v| v.is_object()) else {
        return fail("Invalid worktree binding");
    };
    if record.get("sha256").and_then(Value::as_str)
        != Some(&sha256_hex(canonical_json_value(value)?.as_bytes()))
    {
        return fail("Worktree binding digest mismatch");
    }
    let (Some(task_id), Some(baseline), Some(branch)) = (
        str_field(value, "task_id"),
        str_field(value, "baseline"),
        str_field(value, "branch"),
    ) else {
        return fail("Invalid worktree binding identity");
    };
    let paths: Option<Vec<String>> =
        value
            .get("owned_paths")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .map(|p| p.as_str().map(str::to_owned))
                    .collect()
            });
    let Some(owned_paths) = paths else {
        return fail("Invalid worktree ownership paths");
    };
    let binding = WorktreeBinding {
        task_id,
        baseline,
        branch,
        owned_paths,
    };
    if binding.task_id != brief.task_id || binding.owned_paths != brief.owned_paths {
        return fail("Worktree scope disagrees with task brief");
    }
    Ok(binding)
}

/// Create one worktree and begin its durable task, without dispatching agents.
///
/// If evidence creation fails, the worktree is intentionally retained for manual
/// inspection; never clean up automatically after an ambiguous write.
///
/// # Errors
/// The task binding or reservation is invalid, or worktree creation or evidence persistence fails.
pub fn prepare_worktree(task: &TaskWorkspace, repo: &Path) -> Res<WorktreeBinding> {
    let (root, brief) = (&task.state_root, &task.brief);
    let spec = WorktreeSpec {
        ticket_id: brief.task_id.clone(),
        branch: format!("harness/{}", brief.task_id),
        owned_paths: brief.owned_paths.clone(),
    };
    let reservation = reserve(task, repo)?;
    fence(task, "preparing")?;
    let created = create_worktree(repo, &task.workspace, &spec)?;
    let initial = audit_worktree(&created, "HEAD", &brief.owned_paths)?;
    if !initial.changed_paths.is_empty() {
        return fail("New worktree is unexpectedly dirty");
    }
    if initial.head != reservation.baseline {
        return fail("Worktree baseline differs from reserved source");
    }
    let binding = WorktreeBinding {
        task_id: brief.task_id.clone(),
        baseline: initial.head,
        branch: spec.branch,
        owned_paths: brief.owned_paths.clone(),
    };
    begin_reserved(root, brief)?;
    let sha256 = sha256_hex(canonical_json(&binding)?.as_bytes());
    write_once_value(
        &binding_path(root, brief)?,
        &serde_json::json!({ "binding": binding, "sha256": sha256 }),
    )?;
    fence(task, "prepared")?;
    Ok(binding)
}

fn audit_record(workspace: &Path, audit: &WorktreeAudit) -> Res<Value> {
    Ok(serde_json::json!({ "audit": audit, "content_sha256": snapshot(workspace, audit)? }))
}

/// Call a controlled offline double, then record independent Git audit evidence.
///
/// # Errors
/// Task ownership or phase is invalid, the worker fails, or audit or evidence persistence fails.
pub fn delegate_in_worktree(task: &TaskWorkspace, worker: &mut dyn OfflineAgent) -> Res<Finding> {
    let (root, workspace, brief) = (&task.state_root, &task.workspace, &task.brief);
    let binding = load_binding(root, brief)?;
    if phase(root, brief)? != Phase::Created {
        return fail("Worker may only run once");
    }
    {
        let _lock = task_lock(root, &brief.task_id)?;
        if reconcile(task)? != RecoveryState::Prepared {
            return fail("Dispatch requires prepared reservation without prior dispatch");
        }
        fence(task, "dispatch")?;
    }
    let result = delegate(root, brief, worker)?;
    let audit = audit_worktree(workspace, &binding.baseline, &binding.owned_paths)?;
    write_once_value(
        &root.join(&brief.task_id).join("worktree-audit.json"),
        &audit_record(workspace, &audit)?,
    )?;
    Ok(result)
}

/// Re-audit current worktree; reject stale or altered evidence.
///
/// # Errors
/// Worktree binding, baseline, ownership, or recorded snapshot verification fails.
pub fn verified_audit(task: &TaskWorkspace) -> Res<WorktreeAudit> {
    let (root, workspace, brief) = (&task.state_root, &task.workspace, &task.brief);
    let binding = load_binding(root, brief)?;
    if reconcile(task)? != RecoveryState::WorkerRecorded {
        return fail("Incomplete or uncertain dispatch evidence");
    }
    if phase(root, brief)? == Phase::Created {
        return fail("Worker evidence is missing");
    }
    let stored = read(&root.join(&brief.task_id).join("worktree-audit.json"))?;
    let current = audit_worktree(workspace, &binding.baseline, &binding.owned_paths)?;
    let current_record = audit_record(workspace, &current)?;
    if current.baseline != binding.baseline
        || canonical_json_value(&stored)? != canonical_json_value(&current_record)?
    {
        return fail("Worktree evidence changed after worker audit");
    }
    Ok(current)
}

/// Require independent Git audit before admitting separate reviewer output.
///
/// # Errors
/// Worker audit or task phase is invalid, the reviewer fails, or review evidence cannot be
/// persisted.
pub fn review_worktree(task: &TaskWorkspace, reviewer: &mut dyn OfflineAgent) -> Res<Finding> {
    verified_audit(task)?;
    {
        let _lock = task_lock(&task.state_root, &task.brief.task_id)?;
        if task
            .state_root
            .join(&task.brief.task_id)
            .join("review_dispatch.json")
            .exists()
        {
            return fail("Reviewer already dispatched or uncertain");
        }
        fence(task, "review_dispatch")?;
    }
    Ok(review(&task.state_root, &task.brief, reviewer)?)
}

/// Require unchanged audited evidence before recording unapproved request.
///
/// # Errors
/// Review or worktree evidence is invalid, or approval request persistence fails.
pub fn request_worktree_approval(task: &TaskWorkspace) -> Res<PathBuf> {
    verified_audit(task)?;
    Ok(request_approval(&task.state_root, &task.brief)?)
}
