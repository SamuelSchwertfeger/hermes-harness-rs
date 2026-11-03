//! Fail-closed Hermes invocation planning; no process or gateway is launched.
//!
//! Hermes CLI lifetime cannot be used as proof of completion of gateway-owned work.
//! Keep execution disabled until the installed runtime exposes verifiable cancellation.

use crate::util::PyStr as _;
use crate::util::create_private_dir_all as create_private_dir;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Maximum prompt length in characters.
pub const MAX_PROMPT_CHARS: usize = 8_000;

/// Errors raised by the adapter.
#[derive(Debug, thiserror::Error)]
pub enum HermesAdapterError {
    /// An unsafe or unverified Hermes action was requested.
    #[error("{0}")]
    Safety(String),
    /// Invalid request (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// Missing authorization (Python `PermissionError`).
    #[error("{0}")]
    Permission(String),
    /// Refused to overwrite an existing file (Python `FileExistsError`).
    #[error("{0}")]
    FileExists(String),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Serialization failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

crate::util::wire_enum! {
    /// Kind of delegated task.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum TaskKind {
        /// Scouting task.
        Scout => "scout",
        /// Planning task.
        Plan => "plan",
        /// Review task.
        Review => "review",
        /// Cyber task.
        Cyber => "cyber",
    }

    /// The Python enum value.
    as_str;
}

/// Provider and model for each routable kind.
pub const ROUTES: [(TaskKind, &str, &str); 3] = [
    (TaskKind::Scout, "ollama", "granite4:3b"),
    (TaskKind::Plan, "openai-codex", "gpt-6.1-sol"),
    (TaskKind::Review, "openai-codex", "gpt-6.1-sol"),
];

/// A delegated task request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRequest {
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
    /// Explicit authorized scope (required for cyber tasks).
    pub authorized_scope: Option<String>,
}

impl TaskRequest {
    /// Build a request with the default budgets (60 seconds, 2 turns) and no scope.
    #[must_use]
    pub fn new(ticket_id: &str, kind: TaskKind, prompt: &str, workspace: &Path) -> Self {
        Self {
            ticket_id: ticket_id.to_owned(),
            kind,
            prompt: prompt.to_owned(),
            workspace: workspace.to_path_buf(),
            deadline_seconds: 60,
            max_turns: 2,
            authorized_scope: None,
        }
    }

    /// Check every request invariant.
    ///
    /// # Errors
    /// The task identity, kind, prompt, or paths violate the bounded adapter policy.
    pub fn validate(&self) -> Result<(), HermesAdapterError> {
        let value = |msg: &str| Err(HermesAdapterError::Value(msg.to_owned()));
        if self.ticket_id.is_empty()
            || self.ticket_id.contains('/')
            || self.ticket_id.contains("..")
        {
            return value("Invalid ticket ID");
        }
        if self.prompt.py_strip().is_empty() || self.prompt.chars().count() > MAX_PROMPT_CHARS {
            return value("Prompt must contain 1 to 8000 characters");
        }
        if self.deadline_seconds <= 0 || self.max_turns <= 0 {
            return value("Time and turn budgets must be positive");
        }
        if !self.workspace.is_absolute() || !self.workspace.is_dir() {
            return value("Workspace must be an existing absolute directory");
        }
        if self.kind == TaskKind::Cyber
            && self.authorized_scope.as_deref().is_none_or(str::is_empty)
        {
            return Err(HermesAdapterError::Permission(
                "Cyber tasks require an explicit authorized scope".to_owned(),
            ));
        }
        Ok(())
    }
}

/// A prepared, audited invocation plan (execution stays disabled).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedInvocation {
    /// Ticket identity.
    pub ticket_id: String,
    /// Task kind value.
    pub kind: String,
    /// Provider name.
    pub provider: String,
    /// Model name.
    pub model: String,
    /// Turn budget.
    pub max_turns: i64,
    /// Deadline in seconds.
    pub deadline_seconds: i64,
    /// Resolved workspace path.
    pub workspace: String,
    /// Prompt text.
    pub prompt: String,
    /// Whether execution is enabled (always false).
    pub execution_enabled: bool,
    /// Why execution is disabled.
    pub disabled_reason: String,
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps, clippy::missing_const_for_fn)] // same signature as the unix variant
fn restrict_file(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

impl PreparedInvocation {
    /// Persist a plan, never any Hermes credentials or full configuration.
    ///
    /// # Errors
    /// The request violates the bounded adapter policy or evidence cannot be encoded or persisted.
    pub fn save(&self, destination: &Path) -> Result<(), HermesAdapterError> {
        if destination.exists() {
            return Err(HermesAdapterError::FileExists(
                "Refusing to overwrite existing invocation manifest".to_owned(),
            ));
        }
        if let Some(parent) = destination.parent() {
            create_private_dir(parent)?;
        }
        // Prompts may contain private task material; manifests are private by default.
        let mut stream = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        restrict_file(destination)?;
        let body = crate::util::pretty_json_ordered(self)?;
        stream.write_all(format!("{body}\n").as_bytes())?;
        Ok(())
    }
}

/// Prepare an audited task request; no model calls or shell execution.
///
/// # Errors
/// The request violates the bounded adapter policy or evidence cannot be encoded or persisted.
pub fn prepare(request: &TaskRequest) -> Result<PreparedInvocation, HermesAdapterError> {
    request.validate()?;
    let Some((_, provider, model)) = ROUTES.iter().find(|(kind, _, _)| *kind == request.kind)
    else {
        return Err(HermesAdapterError::Safety(
            "Cyber adapter disabled pending authorization and model benchmark".to_owned(),
        ));
    };
    let workspace =
        fs::canonicalize(&request.workspace).unwrap_or_else(|_| request.workspace.clone());
    Ok(PreparedInvocation {
        ticket_id: request.ticket_id.clone(),
        kind: request.kind.as_str().to_owned(),
        provider: (*provider).to_owned(),
        model: (*model).to_owned(),
        max_turns: request.max_turns,
        deadline_seconds: request.deadline_seconds,
        workspace: workspace.to_string_lossy().into_owned(),
        prompt: request.prompt.clone(),
        execution_enabled: false,
        disabled_reason: "Gateway job identity and cancellation not verified".to_owned(),
    })
}

/// Hard-stop until tested gateway-aware lifecycle controls are installed.
///
/// # Errors
/// The request violates the bounded adapter policy or evidence cannot be encoded or persisted.
pub fn execute(invocation: &PreparedInvocation) -> Result<(), HermesAdapterError> {
    Err(HermesAdapterError::Safety(format!(
        "Hermes execution disabled: {}",
        invocation.disabled_reason
    )))
}
