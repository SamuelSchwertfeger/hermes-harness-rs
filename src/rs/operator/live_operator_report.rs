//! Read-only cross-ledger recovery report for supervised live jobs.
//!
//! Reports are observations, never authority to launch, retry, approve, or merge.
//! Opening `SQLite` in read-only mode avoids silent database creation and mutation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, Row, ToSql};
use serde::Serialize;

use crate::util::{JsonError, pretty_json_ordered};

/// The operator cannot trust the supplied ledger set.
#[derive(Debug, thiserror::Error)]
pub enum LiveReportError {
    /// The ledger set was refused.
    #[error("{0}")]
    Refused(String),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

fn refuse(message: &str) -> LiveReportError {
    LiveReportError::Refused(message.to_owned())
}

/// Exact, explicitly selected on-host ledger files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerPaths {
    /// Execution bindings ledger.
    pub bindings: PathBuf,
    /// Service journal.
    pub services: PathBuf,
    /// Admission ledger.
    pub admissions: PathBuf,
}

/// Durable observations for a single task role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleReport {
    /// `worker` or `reviewer`.
    pub role: String,
    /// Service unit.
    pub unit: Option<String>,
    /// Submission identity.
    pub submission_id: Option<String>,
    /// Gateway session identity.
    pub session_id: Option<String>,
    /// Recorded service state.
    pub service_state: Option<String>,
    /// Recorded admission state.
    pub admission_state: Option<String>,
    /// Gateway admission identity.
    pub admission_id: Option<String>,
    /// Reasons the observation cannot be trusted.
    pub concerns: Vec<String>,
}

/// Non-authoritative recovery summary with no RPC or state transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskReport {
    /// Task identity.
    pub task_id: String,
    /// Worker then reviewer.
    pub roles: Vec<RoleReport>,
    /// Always false.
    pub safe_to_dispatch: bool,
    /// Always false.
    pub safe_to_retry: bool,
    /// Always false.
    pub approved: bool,
}

impl TaskReport {
    /// Serialize only allowlisted fields; do not include prompts or credentials.
    ///
    /// # Errors
    /// The value cannot be serialized by the Python-compatible JSON encoder.
    pub fn to_json(&self) -> Result<String, JsonError> {
        pretty_json_ordered(self)
    }
}

struct BindingRow {
    role: String,
    submission_id: String,
    session_id: String,
    unit: String,
}

struct ServiceRow {
    role: String,
    state: String,
}

struct AdmissionRow {
    session_id: String,
    admission_id: Option<String>,
    local_state: String,
}

/// Fail closed on missing paths, symlinks, directories, or `SQLite` failures.
fn open_readonly(path: &Path) -> Result<Connection, LiveReportError> {
    if crate::util::lstat(path).is_ok_and(|m| m.file_type().is_symlink()) || !path.is_file() {
        return Err(refuse(
            "Ledger must be an existing regular non-symlink file",
        ));
    }
    // Reject symlinked ancestors too, without following untrusted directory links.
    let linked_parent = path.ancestors().skip(1).any(|parent| {
        !parent.as_os_str().is_empty()
            && crate::util::lstat(parent).is_ok_and(|m| m.file_type().is_symlink())
    });
    if linked_parent {
        return Err(refuse("Symlinked ledger parent is forbidden"));
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| refuse("Cannot open operator ledger read-only"))
}

fn rows<T>(
    path: &Path,
    query: &str,
    param: &str,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, LiveReportError> {
    let connection = open_readonly(path)?;
    let fetched = (|| {
        let mut statement = connection.prepare(query)?;
        let params: [&dyn ToSql; 1] = [&param];
        statement
            .query_map(params.as_slice(), map)?
            .collect::<rusqlite::Result<Vec<T>>>()
    })();
    fetched.map_err(|_| refuse("Invalid or unreadable operator ledger schema"))
}

fn service_concerns(role: &str, service: Option<&ServiceRow>) -> Vec<String> {
    let Some(service) = service else {
        return vec!["service_missing".to_owned()];
    };
    let mut concerns = Vec::new();
    if service.role != role {
        concerns.push("service_role_mismatch".to_owned());
    }
    if service.state != "running_observed" {
        concerns.push("service_not_recorded_running".to_owned());
    }
    concerns
}

fn admission_concerns(binding: &BindingRow, admission: Option<&AdmissionRow>) -> Vec<String> {
    let Some(admission) = admission else {
        return vec!["admission_missing".to_owned()];
    };
    let mut concerns = Vec::new();
    if admission.session_id != binding.session_id {
        concerns.push("session_mismatch".to_owned());
    }
    let state = admission.local_state.as_str();
    if matches!(state, "prepared" | "outcome_unknown") {
        concerns.push("submission_uncertain_or_pending".to_owned());
    }
    if !matches!(
        state,
        "prepared" | "outcome_unknown" | "admitted" | "receipt_observed"
    ) {
        concerns.push("admission_state_unrecognized".to_owned());
    }
    concerns
}

fn role_report(
    role: &str,
    binding: Option<&BindingRow>,
    service: Option<&ServiceRow>,
    admission: Option<&AdmissionRow>,
) -> RoleReport {
    let Some(binding) = binding else {
        return RoleReport {
            role: role.to_owned(),
            unit: None,
            submission_id: None,
            session_id: None,
            service_state: None,
            admission_state: None,
            admission_id: None,
            concerns: vec!["binding_missing".to_owned()],
        };
    };
    let mut concerns = service_concerns(role, service);
    concerns.extend(admission_concerns(binding, admission));
    // A previously active record is not current process liveness.
    concerns.push("live_process_not_verified".to_owned());
    RoleReport {
        role: role.to_owned(),
        unit: Some(binding.unit.clone()),
        submission_id: Some(binding.submission_id.clone()),
        session_id: Some(binding.session_id.clone()),
        service_state: service.map(|s| s.state.clone()),
        admission_state: admission.map(|a| a.local_state.clone()),
        admission_id: admission.and_then(|a| a.admission_id.clone()),
        concerns,
    }
}

fn load_bindings(
    paths: &LedgerPaths,
    task_id: &str,
) -> Result<HashMap<String, BindingRow>, LiveReportError> {
    let bindings = rows(
        &paths.bindings,
        "SELECT task_id, role, submission_id, session_id, unit FROM bindings \
         WHERE task_id = ? ORDER BY role",
        task_id,
        |row| {
            Ok(BindingRow {
                role: row.get("role")?,
                submission_id: row.get("submission_id")?,
                session_id: row.get("session_id")?,
                unit: row.get("unit")?,
            })
        },
    )?;
    let count = bindings.len();
    let by_role: HashMap<String, BindingRow> = bindings
        .into_iter()
        .map(|binding| (binding.role.clone(), binding))
        .collect();
    if by_role.len() != count
        || by_role
            .keys()
            .any(|r| !matches!(r.as_str(), "worker" | "reviewer"))
    {
        return Err(refuse("Invalid task-role bindings"));
    }
    Ok(by_role)
}

fn observe_role(
    paths: &LedgerPaths,
    role: &str,
    binding: Option<&BindingRow>,
) -> Result<RoleReport, LiveReportError> {
    let (mut service, mut admission) = (None, None);
    if let Some(binding) = binding {
        let mut services = rows(
            &paths.services,
            "SELECT unit, role, state FROM services WHERE unit = ?",
            &binding.unit,
            |row| {
                Ok(ServiceRow {
                    role: row.get("role")?,
                    state: row.get("state")?,
                })
            },
        )?;
        let mut admissions = rows(
            &paths.admissions,
            "SELECT submission_id, session_id, admission_id, local_state \
             FROM admissions WHERE submission_id = ?",
            &binding.submission_id,
            |row| {
                Ok(AdmissionRow {
                    session_id: row.get("session_id")?,
                    admission_id: row.get("admission_id")?,
                    local_state: row.get("local_state")?,
                })
            },
        )?;
        if services.len() > 1 || admissions.len() > 1 {
            return Err(refuse("Duplicate durable identity"));
        }
        service = services.pop();
        admission = admissions.pop();
    }
    Ok(role_report(
        role,
        binding,
        service.as_ref(),
        admission.as_ref(),
    ))
}

/// Join three durable ledgers without constructing mutable stores or doing RPC.
///
/// The result cannot authorize execution even when all three ledgers agree.
///
/// # Errors
/// Task, admission, or service ledger data cannot be read or encoded.
pub fn inspect_task(paths: &LedgerPaths, task_id: &str) -> Result<TaskReport, LiveReportError> {
    if crate::broker::live_capability_broker::is_blank(task_id) {
        return Err(refuse("Nonblank task identity required"));
    }
    let by_role = load_bindings(paths, task_id)?;
    let mut roles = Vec::new();
    for role in ["worker", "reviewer"] {
        roles.push(observe_role(paths, role, by_role.get(role))?);
    }
    Ok(TaskReport {
        task_id: task_id.to_owned(),
        roles,
        safe_to_dispatch: false,
        safe_to_retry: false,
        approved: false,
    })
}
