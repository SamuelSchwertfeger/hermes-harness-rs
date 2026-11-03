//! Durable, fail-closed service launch journal independent of gateway admissions.

use crate::util::PathLexical as _;
use crate::util::ensure_private_leaf_dir as create_private_dir;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};

/// A service identity cannot be safely recorded or transitioned.
#[derive(Debug, thiserror::Error)]
pub enum ServiceJournalError {
    /// A journal rule was violated.
    #[error("{0}")]
    Refused(String),
    /// SQLite failure.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
}

crate::util::wire_enum! {
    /// What the controller knows, not an inferred systemd state.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum ServiceState {
        /// Reserved; launch outcome not verified.
        LaunchUncertain => "launch_uncertain",
        /// Active service was observed.
        RunningObserved => "running_observed",
        /// Stop requested; outcome not verified.
        StopUncertain => "stop_uncertain",
        /// Inactive service was observed.
        StoppedObserved => "stopped_observed",
    }

    /// The Python string value of the state.
    as_str;
}

impl ServiceState {
    fn parse(value: &str) -> Result<Self, ServiceJournalError> {
        match value {
            "launch_uncertain" => Ok(Self::LaunchUncertain),
            "running_observed" => Ok(Self::RunningObserved),
            "stop_uncertain" => Ok(Self::StopUncertain),
            "stopped_observed" => Ok(Self::StoppedObserved),
            other => Err(ServiceJournalError::Refused(format!(
                "{other:?} is not a valid ServiceState"
            ))),
        }
    }
}

/// Durable service binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRecord {
    /// Unit name (primary key).
    pub unit: String,
    /// Role string.
    pub role: String,
    /// Recorded plan JSON.
    pub plan_json: String,
    /// Known state.
    pub state: ServiceState,
}

/// SQLite records kept even when a launch or cancellation outcome is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceJournal {
    /// Database file path.
    pub path: PathBuf,
}

fn non_empty(path: &Path) -> Option<&Path> {
    Some(path).filter(|p| !p.as_os_str().is_empty())
}

impl ServiceJournal {
    /// Open or create the journal (parent dir mode 0700, file mode 0600 on unix).
    ///
    /// # Errors
    /// The unit reservation or state transition conflicts, or filesystem or database access fails.
    pub fn new(path: &Path) -> Result<Self, ServiceJournalError> {
        if path.is_symlink_lexical() {
            return Err(ServiceJournalError::Refused(
                "Journal symlinks are not permitted".to_owned(),
            ));
        }
        if let Some(parent) = path.parent().and_then(non_empty) {
            create_private_dir(parent)?;
        }
        let journal = Self {
            path: path.to_path_buf(),
        };
        journal.connect()?.execute(
            "CREATE TABLE IF NOT EXISTS services (\
             unit TEXT PRIMARY KEY, role TEXT NOT NULL, \
             plan_json TEXT NOT NULL, state TEXT NOT NULL)",
            [],
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(journal)
    }

    fn connect(&self) -> Result<Connection, ServiceJournalError> {
        let connection = crate::util::open_sqlite(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
    }

    /// Fence an identity before any external side effects.
    ///
    /// # Errors
    /// The unit reservation or state transition conflicts, or filesystem or database access fails.
    pub fn reserve(
        &self,
        unit: &str,
        role: &str,
        plan_json: &str,
    ) -> Result<ServiceRecord, ServiceJournalError> {
        let inserted = self.connect()?.execute(
            "INSERT INTO services (unit, role, plan_json, state) VALUES (?, ?, ?, ?)",
            params![
                unit,
                role,
                plan_json,
                ServiceState::LaunchUncertain.as_str()
            ],
        );
        match inserted {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == ErrorCode::ConstraintViolation =>
            {
                return Err(ServiceJournalError::Refused(
                    "Service identity already reserved; never relaunch".to_owned(),
                ));
            }
            Err(err) => return Err(err.into()),
        }
        self.get(unit)?
            .ok_or_else(|| ServiceJournalError::Refused("Reserved service disappeared".to_owned()))
    }

    /// Read without changing the durable record.
    ///
    /// # Errors
    /// The unit reservation or state transition conflicts, or filesystem or database access fails.
    pub fn get(&self, unit: &str) -> Result<Option<ServiceRecord>, ServiceJournalError> {
        let row = self
            .connect()?
            .query_row(
                "SELECT unit, role, plan_json, state FROM services WHERE unit = ?",
                params![unit],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(unit, role, plan_json, state)| {
            Ok(ServiceRecord {
                unit,
                role,
                plan_json,
                state: ServiceState::parse(&state)?,
            })
        })
        .transpose()
    }

    /// Atomic compare-and-swap transition; no optimistic retries.
    ///
    /// # Errors
    /// The unit reservation or state transition conflicts, or filesystem or database access fails.
    pub fn transition(
        &self,
        unit: &str,
        expected: ServiceState,
        target: ServiceState,
    ) -> Result<(), ServiceJournalError> {
        let changed = self.connect()?.execute(
            "UPDATE services SET state = ? WHERE unit = ? AND state = ?",
            params![target.as_str(), unit, expected.as_str()],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(ServiceJournalError::Refused(
                "Service transition refused; inspect durable state".to_owned(),
            ))
        }
    }
}
