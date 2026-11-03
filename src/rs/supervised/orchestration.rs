//! Fail-closed, auditable orchestration of *injected* offline worker results.
//!
//! No Hermes or Ollama network adapter is authorized by this module. A caller must
//! supply an explicitly trusted Worker implementation. The default has no dispatch.

use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::delegation::{create_new_private, create_private_dir};
use crate::core::state::{Stage, StateError, load};
use crate::core::transport::{ForegroundFixtureWorker, TransportError};
use crate::gateway::hermes_adapter::{
    HermesAdapterError, PreparedInvocation, TaskKind, TaskRequest, prepare,
};
use crate::util::{JsonError, PyStr as _, pretty_json_ordered, utc_now_iso, uuid4_hex};

/// Largest accepted worker output in characters.
pub const MAX_RESPONSE_CHARS: usize = 40_000;

crate::util::wire_enum! {
    /// Outcome recorded for a dispatch.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
    pub enum TaskStatus {
        /// The worker returned valid output.
        Succeeded => "succeeded",
        /// The worker or output validation failed.
        Failed => "failed",
    }

    /// Serialized lowercase status.
    as_str;
}

/// A worker failure with the Python exception class name that is persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerFailure {
    /// Name of the failing error type (Python `type(exc).__name__`).
    pub error_type: String,
    /// Human-readable failure text; never persisted.
    pub message: String,
}

impl fmt::Display for WorkerFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkerFailure {}

/// Worker dispatch forbidden or evidence validation failed.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    /// Dispatch refused or output invalid.
    #[error("{0}")]
    Dispatch(String),
    /// The injected worker failed.
    #[error(transparent)]
    Worker(#[from] WorkerFailure),
    /// Ticket state failure.
    #[error(transparent)]
    State(#[from] StateError),
    /// Adapter validation failure.
    #[error(transparent)]
    Adapter(#[from] HermesAdapterError),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

fn fail<T>(msg: &str) -> Result<T, DispatchError> {
    Err(DispatchError::Dispatch(msg.to_owned()))
}

/// Trusted offline worker seam; never automatically constructs a live backend.
pub trait Worker {
    /// Return bounded, tool-free text for a single approved invocation.
    fn run(&self, invocation: &PreparedInvocation) -> Result<String, WorkerFailure>;
}

impl Worker for ForegroundFixtureWorker<'_> {
    fn run(&self, invocation: &PreparedInvocation) -> Result<String, WorkerFailure> {
        Self::run(self, invocation).map_err(|error| WorkerFailure {
            error_type: match error {
                TransportError::Transport(_) => "TransportError",
                TransportError::Value(_) => "ValueError",
                TransportError::Permission(_) => "PermissionError",
                TransportError::Watchdog(_) => "WatchdogError",
                TransportError::Io(_) => "OSError",
                TransportError::Encode(_) => "TypeError",
            }
            .to_owned(),
            message: error.to_string(),
        })
    }
}

/// One approved, bounded task request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSpec {
    /// Ticket identity.
    pub ticket_id: String,
    /// Task kind.
    pub kind: TaskKind,
    /// Prompt text.
    pub prompt: String,
    /// Absolute workspace directory.
    pub workspace: PathBuf,
    /// Deadline in seconds.
    pub deadline_seconds: i64,
    /// Turn budget.
    pub max_turns: i64,
}

impl TaskSpec {
    /// Build a spec with the default budgets (30 seconds, 2 turns).
    #[must_use]
    pub fn new(ticket_id: &str, kind: TaskKind, prompt: &str, workspace: &Path) -> Self {
        Self {
            ticket_id: ticket_id.to_owned(),
            kind,
            prompt: prompt.to_owned(),
            workspace: workspace.to_path_buf(),
            deadline_seconds: 30,
            max_turns: 2,
        }
    }
}

/// Retained, private record of one successful dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskResult {
    /// Run identity.
    pub run_id: String,
    /// Ticket identity.
    pub ticket_id: String,
    /// Task kind value.
    pub kind: String,
    /// Provider name.
    pub provider: String,
    /// Model name.
    pub model: String,
    /// Outcome.
    pub status: TaskStatus,
    /// Path of the retained output text.
    pub output_path: String,
    /// UTC completion timestamp.
    pub timestamp: String,
}

#[derive(Serialize)]
struct FailureRecord {
    status: TaskStatus,
    error_type: String,
}

const fn allowed_stages(kind: TaskKind) -> &'static [Stage] {
    match kind {
        TaskKind::Scout => &[Stage::Investigate, Stage::Facts],
        TaskKind::Plan => &[Stage::Ticket, Stage::Plan],
        TaskKind::Review => &[Stage::Review],
        TaskKind::Cyber => &[],
    }
}

/// Create without overwrite and avoid exposing private task data to other users.
///
/// # Errors
/// The evidence directory or file cannot be created, encoded, or written.
fn save_private(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut stream = create_new_private(path)?;
    let written = stream
        .write_all(content.as_bytes())
        .and_then(|()| stream.flush())
        .and_then(|()| stream.sync_all());
    if written.is_err() {
        drop(fs::remove_file(path));
    }
    written
}

/// Reject empty or over-budget worker output.
///
/// # Errors
/// The worker output is empty or exceeds the character budget.
fn validate_output(output: String) -> Result<String, DispatchError> {
    if output.py_strip().is_empty() || output.chars().count() > MAX_RESPONSE_CHARS {
        return fail("Worker output missing or over byte-independent character budget");
    }
    Ok(output)
}

fn is_ticket_id(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Validate ticket stage, kind, workspace and identity, then prepare the invocation.
///
/// # Errors
/// Task validation, invocation preparation, or evidence persistence fails.
fn preflight(root: &Path, spec: &TaskSpec) -> Result<PreparedInvocation, DispatchError> {
    let ticket = load(root, &spec.ticket_id)?;
    if spec.kind == TaskKind::Cyber {
        return fail("Cyber and unknown workers are not authorized");
    }
    if !allowed_stages(spec.kind).contains(&ticket.stage) {
        return Err(DispatchError::Dispatch(format!(
            "Worker {} not allowed at ticket stage {}",
            spec.kind.as_str(),
            ticket.stage.as_str()
        )));
    }
    if !spec.workspace.is_dir() || !spec.workspace.is_absolute() {
        return fail("Workspace must be an existing absolute directory");
    }
    if !is_ticket_id(&spec.ticket_id) {
        return fail("Invalid ticket ID");
    }
    let mut request = TaskRequest::new(&spec.ticket_id, spec.kind, &spec.prompt, &spec.workspace);
    request.deadline_seconds = spec.deadline_seconds;
    request.max_turns = spec.max_turns;
    Ok(prepare(&request)?)
}

fn run_worker(
    worker: &dyn Worker,
    invocation: &PreparedInvocation,
    directory: &Path,
) -> Result<String, DispatchError> {
    let outcome = worker
        .run(invocation)
        .map_err(DispatchError::from)
        .and_then(validate_output);
    if let Err(error) = &outcome {
        let error_type = match error {
            DispatchError::Worker(failure) => failure.error_type.clone(),
            DispatchError::Dispatch(_) => "DispatchError".to_owned(),
            DispatchError::State(_) => "ValueError".to_owned(),
            DispatchError::Adapter(_) => "HermesAdapterError".to_owned(),
            DispatchError::Io(_) => "OSError".to_owned(),
            DispatchError::Encode(_) => "TypeError".to_owned(),
        };
        let record = FailureRecord {
            status: TaskStatus::Failed,
            error_type,
        };
        let body = format!("{}\n", pretty_json_ordered(&record)?);
        save_private(&directory.join("failure.json"), &body)?;
    }
    outcome
}

/// Run a trusted mock/offline worker and retain a private auditable result.
///
/// This is NOT a timeout or sandbox boundary. Live adapters must not use this API
/// until gateway cancellation and enforced deadline controls are integrated.
///
/// # Errors
/// Task validation, invocation preparation, or evidence persistence fails.
pub fn dispatch_offline(
    root: &Path,
    spec: &TaskSpec,
    worker: &dyn Worker,
) -> Result<TaskResult, DispatchError> {
    let invocation = preflight(root, spec)?;
    let run_id = uuid4_hex()?;
    let runs = root.join("tickets").join(&spec.ticket_id).join("runs");
    fs::create_dir_all(&runs)?;
    let directory = runs.join(&run_id);
    create_private_dir(&directory)?;
    invocation.save(&directory.join("request.json"))?;
    let output = run_worker(worker, &invocation, &directory)?;
    let output_path = directory.join("output.txt");
    save_private(&output_path, &output)?;
    let result = TaskResult {
        run_id,
        ticket_id: spec.ticket_id.clone(),
        kind: spec.kind.as_str().to_owned(),
        provider: invocation.provider,
        model: invocation.model,
        status: TaskStatus::Succeeded,
        output_path: output_path.to_string_lossy().into_owned(),
        timestamp: utc_now_iso(),
    };
    let body = format!("{}\n", pretty_json_ordered(&result)?);
    save_private(&directory.join("result.json"), &body)?;
    Ok(result)
}
