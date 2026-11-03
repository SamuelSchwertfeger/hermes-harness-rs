//! Offline-only delegated worker/reviewer workflow with durable, fail-closed stages.
//!
//! This API deliberately does not start agents, execute subprocesses, merge, or grant
//! approval. Caller-supplied implementations must be deterministic offline doubles.

use crate::util::PathLexical as _;
pub(crate) use crate::util::create_private_dir;
use crate::util::is_slug_id;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::{PyStr as _, canonical_json, canonical_json_value, sha256_hex};

/// Maximum characters accepted in an objective or agent output.
pub const MAX_TEXT_CHARS: usize = 40_000;
/// Permission bits of evidence files.
pub const PRIVATE_MODE: u32 = 0o600;

/// Invalid task, evidence, identity, or workflow transition.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DelegationError {
    /// Rule violation.
    #[error("{0}")]
    Message(String),
    /// Failure reported by an injected agent.
    #[error("{0}")]
    Agent(String),
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

fn fail<T>(msg: &str) -> Result<T, DelegationError> {
    Err(DelegationError::Message(msg.to_owned()))
}

crate::util::wire_enum! {
    /// Workflow stage recovered from immutable evidence.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum Phase {
        /// Brief persisted, no worker output.
        Created => "created",
        /// Worker output persisted.
        WorkerDone => "worker_done",
        /// Reviewer output persisted.
        Reviewed => "reviewed",
        /// Approval requested from a human.
        AwaitingApproval => "awaiting_approval",
    }

    /// The Python enum value.
    as_str;
}

/// Immutable task description.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brief {
    /// Task identifier.
    pub task_id: String,
    /// Free-text objective.
    pub objective: String,
    /// Relative paths the worker may change.
    pub owned_paths: Vec<String>,
    /// Worker identity.
    pub worker_id: String,
    /// Reviewer identity (distinct from the worker).
    pub reviewer_id: String,
}

/// Text evidence produced by one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Identity of the producing agent.
    pub agent_id: String,
    /// Evidence text.
    pub text: String,
}

/// Deterministic offline agent double.
pub trait OfflineAgent {
    /// Produce deterministic text evidence with no external side effects.
    fn run(&mut self, brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError>;
}

/// Validate task ID, objective, identities and owned paths.
///
/// # Errors
/// The task ID, objective, worker/reviewer identities, or owned paths are invalid.
pub(crate) fn validate(brief: &Brief) -> Result<(), DelegationError> {
    if !is_slug_id(&brief.task_id) {
        return fail("Invalid task ID");
    }
    if brief.objective.py_strip().is_empty() || brief.objective.chars().count() > MAX_TEXT_CHARS {
        return fail("Invalid objective");
    }
    if brief.worker_id.is_empty()
        || brief.reviewer_id.is_empty()
        || brief.worker_id == brief.reviewer_id
    {
        return fail("Worker and reviewer must have distinct identities");
    }
    if brief.owned_paths.is_empty() {
        return fail("At least one owned path is required");
    }
    for path in &brief.owned_paths {
        let invalid_prefix = path.is_empty() || path.starts_with('/') || path.contains('\\');
        let invalid_parts = path.split('/').any(|p| matches!(p, "" | "." | ".."));
        if invalid_prefix || invalid_parts {
            return fail("Invalid owned path");
        }
    }
    Ok(())
}

/// Create a new owner-only file, failing if it exists (never follows symlinks).
pub(crate) fn create_new_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(PRIVATE_MODE);
    }
    options.open(path)
}

/// Write canonical JSON plus newline exclusively and durably.
pub(crate) fn write_once<T: Serialize + ?Sized>(
    path: &Path,
    data: &T,
) -> Result<(), DelegationError> {
    write_encoded(path, canonical_json(data)?.into_bytes())
}

/// Write an existing JSON tree exclusively and durably, with a trailing newline.
pub(crate) fn write_once_value(path: &Path, data: &Value) -> Result<(), DelegationError> {
    write_encoded(path, canonical_json_value(data)?.into_bytes())
}

fn write_encoded(path: &Path, mut encoded: Vec<u8>) -> Result<(), DelegationError> {
    encoded.push(b'\n');
    let mut handle = create_new_private(path)?;
    handle.write_all(&encoded)?;
    handle.flush()?;
    handle.sync_all()?;
    Ok(())
}

fn read(path: &Path) -> Result<Value, DelegationError> {
    if path.is_symlink_lexical() {
        return fail("Symlink evidence is forbidden");
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if !value.is_object() {
        return fail("Malformed evidence document");
    }
    Ok(value)
}

fn task_dir(root: &Path, brief: &Brief) -> Result<PathBuf, DelegationError> {
    validate(brief)?;
    if root.is_symlink_lexical() || !root.is_dir() {
        return fail("Root must be a real, existing directory");
    }
    let directory = root.join(&brief.task_id);
    if directory.is_symlink_lexical() {
        return fail("Symlink task directory is forbidden");
    }
    Ok(directory)
}

/// SHA-256 of the canonical JSON form of the brief.
///
/// # Errors
/// The brief cannot be encoded by the Python-compatible JSON encoder.
pub fn digest(brief: &Brief) -> Result<String, DelegationError> {
    Ok(sha256_hex(canonical_json(brief)?.as_bytes()))
}

fn brief_record(brief: &Brief) -> Result<Value, DelegationError> {
    Ok(serde_json::json!({ "digest": digest(brief)?, "brief": brief }))
}

/// Persist one immutable task brief; never overwrite an earlier attempt.
///
/// # Errors
/// Task paths, ownership, stage, or approval evidence is invalid, or task state cannot be
/// persisted.
pub fn begin(root: &Path, brief: &Brief) -> Result<(), DelegationError> {
    let directory = task_dir(root, brief)?;
    create_private_dir(&directory)?;
    write_once_value(&directory.join("brief.json"), &brief_record(brief)?)
}

/// Persist brief in a pre-reserved task directory; refuse unreserved state.
///
/// # Errors
/// Task paths, ownership, stage, or approval evidence is invalid, or task state cannot be
/// persisted.
pub fn begin_reserved(root: &Path, brief: &Brief) -> Result<(), DelegationError> {
    let directory = task_dir(root, brief)?;
    let reservation = directory.join("reservation.json");
    if !directory.is_dir() || reservation.is_symlink_lexical() || !reservation.is_file() {
        return fail("Reservation required before initializing delegated task");
    }
    write_once_value(&directory.join("brief.json"), &brief_record(brief)?)
}

/// Recover progress from immutable evidence, rejecting missing prerequisites.
///
/// # Errors
/// Task paths, ownership, stage, or approval evidence is invalid, or task state cannot be
/// persisted.
pub fn phase(root: &Path, brief: &Brief) -> Result<Phase, DelegationError> {
    let directory = task_dir(root, brief)?;
    let saved = read(&directory.join("brief.json"))?;
    if canonical_json_value(&saved)? != canonical_json_value(&brief_record(brief)?)? {
        return fail("Task brief mismatch");
    }
    if directory.join("approval-request.json").exists() {
        if !directory.join("review.json").exists() {
            return fail("Approval request lacks review");
        }
        return Ok(Phase::AwaitingApproval);
    }
    if directory.join("review.json").exists() {
        if !directory.join("worker.json").exists() {
            return fail("Review lacks worker output");
        }
        return Ok(Phase::Reviewed);
    }
    if directory.join("worker.json").exists() {
        return Ok(Phase::WorkerDone);
    }
    Ok(Phase::Created)
}

fn finding(
    agent: &mut dyn OfflineAgent,
    brief: &Brief,
    prior: Option<&Finding>,
) -> Result<Finding, DelegationError> {
    let expected = if prior.is_none() {
        &brief.worker_id
    } else {
        &brief.reviewer_id
    };
    let result = agent.run(brief, prior)?;
    if result.agent_id != *expected {
        return fail("Agent identity mismatch");
    }
    if result.text.py_strip().is_empty() || result.text.chars().count() > MAX_TEXT_CHARS {
        return fail("Empty or oversized agent output");
    }
    Ok(result)
}

/// Run one injected offline worker only at CREATED stage.
///
/// # Errors
/// Task paths, ownership, stage, or approval evidence is invalid, or task state cannot be
/// persisted.
pub fn delegate(
    root: &Path,
    brief: &Brief,
    worker: &mut dyn OfflineAgent,
) -> Result<Finding, DelegationError> {
    if phase(root, brief)? != Phase::Created {
        return fail("Worker already dispatched or task advanced");
    }
    let output = finding(worker, brief, None)?;
    write_once(&task_dir(root, brief)?.join("worker.json"), &output)?;
    Ok(output)
}

/// Run a distinct injected offline reviewer against persisted worker evidence.
///
/// # Errors
/// Task paths, ownership, stage, or approval evidence is invalid, or task state cannot be
/// persisted.
pub fn review(
    root: &Path,
    brief: &Brief,
    reviewer: &mut dyn OfflineAgent,
) -> Result<Finding, DelegationError> {
    if phase(root, brief)? != Phase::WorkerDone {
        return fail("Review requires completed worker evidence");
    }
    let saved = read(&task_dir(root, brief)?.join("worker.json"))?;
    let saved_text = saved.get("text").and_then(Value::as_str);
    let agent_ok = saved.get("agent_id").and_then(Value::as_str) == Some(brief.worker_id.as_str());
    let Some(text) = saved_text.filter(|_| agent_ok) else {
        return fail("Corrupt worker evidence");
    };
    let prior = Finding {
        agent_id: brief.worker_id.clone(),
        text: text.to_owned(),
    };
    let output = finding(reviewer, brief, Some(&prior))?;
    write_once(&task_dir(root, brief)?.join("review.json"), &output)?;
    Ok(output)
}

/// Record a request for human review; never grant approval or merge.
///
/// # Errors
/// Task paths, ownership, stage, or approval evidence is invalid, or task state cannot be
/// persisted.
pub fn request_approval(root: &Path, brief: &Brief) -> Result<PathBuf, DelegationError> {
    if phase(root, brief)? != Phase::Reviewed {
        return fail("Approval request requires independent review");
    }
    let path = task_dir(root, brief)?.join("approval-request.json");
    write_once_value(
        &path,
        &serde_json::json!({ "task_digest": digest(brief)?, "approved": false }),
    )?;
    Ok(path)
}
