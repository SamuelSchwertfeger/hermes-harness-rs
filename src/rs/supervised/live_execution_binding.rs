//! Durable role-to-service bindings; never infer authorization from a service name.

use crate::util::PathLexical as _;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OptionalExtension as _, params};

use super::live_supervised_recovery::create_private_dir;
use crate::services::live_service_journal::{ServiceJournal, ServiceJournalError, ServiceState};

/// A role or service binding cannot be established safely.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionBindingError {
    /// A binding rule was violated.
    #[error("{0}")]
    Refused(String),
    /// The service journal failed.
    #[error(transparent)]
    Journal(#[from] ServiceJournalError),
    /// SQLite failure.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn refuse<T>(msg: &str) -> Result<T, ExecutionBindingError> {
    Err(ExecutionBindingError::Refused(msg.to_owned()))
}

/// Immutable identity linking a task, gateway submission and systemd unit.
///
/// `role` is `"worker"` or `"reviewer"`; any other value is refused when binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionBinding {
    /// Task identity.
    pub task_id: String,
    /// `"worker"` or `"reviewer"`.
    pub role: String,
    /// Gateway submission identity.
    pub submission_id: String,
    /// Gateway session identity.
    pub session_id: String,
    /// Systemd unit name.
    pub unit: String,
}

/// Durable binding registry with unique submissions, units and task roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionBindingStore {
    /// Database file path.
    pub path: PathBuf,
}

impl ExecutionBindingStore {
    /// Open or create the store (parent dir mode 0700, file mode 0600 on unix).
    ///
    /// # Errors
    /// Execution identities conflict, the bound service is not running, or ledger access fails.
    pub fn new(path: &Path) -> Result<Self, ExecutionBindingError> {
        if path.is_symlink_lexical() {
            return refuse("Binding store symlinks are forbidden");
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            create_private_dir(parent)?;
        }
        let store = Self {
            path: path.to_path_buf(),
        };
        store.connect()?.execute(
            "CREATE TABLE IF NOT EXISTS bindings (\
             task_id TEXT NOT NULL, role TEXT NOT NULL, \
             submission_id TEXT NOT NULL UNIQUE, session_id TEXT NOT NULL UNIQUE, \
             unit TEXT NOT NULL UNIQUE, PRIMARY KEY (task_id, role))",
            [],
        )?;
        #[cfg(unix)]
        super::live_supervised_recovery::restrict_file(path)?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, ExecutionBindingError> {
        let connection = crate::util::open_sqlite(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
    }

    /// Record intent before service launch or gateway RPC; never overwrite.
    ///
    /// # Errors
    /// Execution identities conflict, the bound service is not running, or ledger access fails.
    pub fn bind(
        &self,
        binding: &ExecutionBinding,
        journal: &ServiceJournal,
    ) -> Result<(), ExecutionBindingError> {
        if !matches!(binding.role.as_str(), "worker" | "reviewer") {
            return refuse("Unknown role");
        }
        let blank = [
            &binding.task_id,
            &binding.submission_id,
            &binding.session_id,
            &binding.unit,
        ]
        .iter()
        .any(|value| value.is_empty());
        if blank {
            return refuse("Blank binding identity");
        }
        if !binding.unit.starts_with("hermes-job-") {
            return refuse("Unit outside authorized namespace");
        }
        if journal.get(&binding.unit)?.is_some() {
            return refuse("Service unit already used or uncertain");
        }
        let inserted = self.connect()?.execute(
            "INSERT INTO bindings VALUES (?, ?, ?, ?, ?)",
            params![
                binding.task_id,
                binding.role,
                binding.submission_id,
                binding.session_id,
                binding.unit,
            ],
        );
        match inserted {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == ErrorCode::ConstraintViolation =>
            {
                refuse("Duplicate role, session, submission or unit")
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Read only; does not advance service state.
    ///
    /// # Errors
    /// Execution identities conflict, the bound service is not running, or ledger access fails.
    pub fn get(
        &self,
        task_id: &str,
        role: &str,
    ) -> Result<Option<ExecutionBinding>, ExecutionBindingError> {
        let binding = self
            .connect()?
            .query_row(
                "SELECT task_id, role, submission_id, session_id, unit \
                 FROM bindings WHERE task_id = ? AND role = ?",
                params![task_id, role],
                |row| {
                    Ok(ExecutionBinding {
                        task_id: row.get(0)?,
                        role: row.get(1)?,
                        submission_id: row.get(2)?,
                        session_id: row.get(3)?,
                        unit: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(binding)
    }

    /// Require the exact recorded service to be observed running.
    ///
    /// # Errors
    /// Execution identities conflict, the bound service is not running, or ledger access fails.
    pub fn require_running(
        &self,
        binding: &ExecutionBinding,
        journal: &ServiceJournal,
    ) -> Result<(), ExecutionBindingError> {
        if self.get(&binding.task_id, &binding.role)?.as_ref() != Some(binding) {
            return refuse("Gateway/service binding mismatch");
        }
        let Some(service) = journal
            .get(&binding.unit)?
            .filter(|service| service.state == ServiceState::RunningObserved)
        else {
            return refuse("Service not durably observed running");
        };
        if service.role != binding.role {
            return refuse("Service role mismatch");
        }
        Ok(())
    }
}
