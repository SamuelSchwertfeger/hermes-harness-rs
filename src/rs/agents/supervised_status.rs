//! Read-only CLI for retained offline supervised execution evidence.

use crate::util::PathLexical as _;
use std::io;
use std::path::{Path, PathBuf};

use clap::Parser;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value;

use crate::util::pretty_json_ordered;

/// Errors raised while inspecting retained evidence.
#[derive(Debug, thiserror::Error)]
pub enum SupervisedStatusError {
    /// Invalid or forbidden evidence (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Malformed JSON evidence.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Ledger read failure.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

#[allow(clippy::struct_excessive_bools)] // one flag per retained record
/// Presence of each retained evidence record, in report order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidencePresence {
    /// `reservation.json`.
    #[serde(rename = "reservation.json")]
    pub reservation: bool,
    /// `brief.json`.
    #[serde(rename = "brief.json")]
    pub brief: bool,
    /// `worker.json`.
    #[serde(rename = "worker.json")]
    pub worker: bool,
    /// `worktree-audit.json`.
    #[serde(rename = "worktree-audit.json")]
    pub worktree_audit: bool,
    /// `review.json`.
    #[serde(rename = "review.json")]
    pub review: bool,
    /// `approval-request.json`.
    #[serde(rename = "approval-request.json")]
    pub approval_request: bool,
}

/// One ledger row, identities only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdmissionRow {
    /// Client submission identity.
    pub submission_id: String,
    /// Session identity.
    pub session_id: String,
    /// Gateway admission identity, if known.
    pub admission_id: Option<String>,
    /// Local ledger state.
    pub state: String,
}

/// Evidence status for one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupervisedStatus {
    /// Task identity.
    pub task_id: String,
    /// Evidence presence.
    pub evidence: EvidencePresence,
    /// Up to 100 ledger rows ordered by submission.
    pub admissions: Vec<AdmissionRow>,
    /// Approval flag, if an approval request exists.
    pub approved: Option<bool>,
}

fn value_error(msg: &str) -> SupervisedStatusError {
    SupervisedStatusError::Value(msg.to_owned())
}

fn present(state: &Path, name: &str) -> Result<bool, SupervisedStatusError> {
    let path = state.join(name);
    if path.is_symlink_lexical() {
        return Err(value_error("Symlinked evidence record is not permitted"));
    }
    Ok(path.is_file())
}

fn read_admissions(ledger: &Path) -> Result<Vec<AdmissionRow>, SupervisedStatusError> {
    let connection = Connection::open_with_flags(ledger, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = connection.prepare(
        "SELECT submission_id, session_id, admission_id, local_state \
         FROM admissions ORDER BY submission_id LIMIT 100",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(AdmissionRow {
            submission_id: row.get(0)?,
            session_id: row.get(1)?,
            admission_id: row.get(2)?,
            state: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn read_approval(path: &Path) -> Result<bool, SupervisedStatusError> {
    let content: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    content
        .get("approved")
        .and_then(Value::as_bool)
        .ok_or_else(|| value_error("Invalid approval-request evidence"))
}

/// Read immutable records without creating a ledger or touching the gateway.
///
/// # Errors
/// The task state or admission ledger cannot be read or decoded.
pub fn inspect(root: &Path, task_id: &str) -> Result<SupervisedStatus, SupervisedStatusError> {
    let state = root.join("state").join(task_id);
    let ledger = root.join("admissions.sqlite");
    if root.is_symlink_lexical() || state.is_symlink_lexical() || ledger.is_symlink_lexical() {
        return Err(value_error("Symlinked state or ledger is not permitted"));
    }
    let evidence = EvidencePresence {
        reservation: present(&state, "reservation.json")?,
        brief: present(&state, "brief.json")?,
        worker: present(&state, "worker.json")?,
        worktree_audit: present(&state, "worktree-audit.json")?,
        review: present(&state, "review.json")?,
        approval_request: present(&state, "approval-request.json")?,
    };
    let admissions = if ledger.is_file() {
        read_admissions(&ledger)?
    } else {
        Vec::new()
    };
    let approved = if evidence.approval_request {
        Some(read_approval(&state.join("approval-request.json"))?)
    } else {
        None
    };
    Ok(SupervisedStatus {
        task_id: task_id.to_owned(),
        evidence,
        admissions,
        approved,
    })
}

/// Command-line arguments of the read-only status tool.
#[derive(Debug, Parser)]
#[command(
    about = "Read-only offline supervised status",
    args_override_self = true,
    infer_long_args = true
)]
pub struct Args {
    /// Evidence root directory.
    #[arg(long)]
    pub root: PathBuf,
    /// Task identity to inspect.
    #[arg(long, default_value = "scripted-demo")]
    pub task_id: String,
}

/// Render evidence status as JSON, with no RPC or write side effects.
///
/// # Errors
/// The status cannot be serialized by the Python-compatible JSON encoder.
pub fn render(args: &Args) -> Result<String, SupervisedStatusError> {
    Ok(pretty_json_ordered(&inspect(&args.root, &args.task_id)?)?)
}
