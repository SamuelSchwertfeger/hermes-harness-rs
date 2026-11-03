//! Durable, local-only records for reconciling Hermes gateway admissions.

use crate::util::PyStr as _;
use crate::util::create_private_dir_all;
use std::fmt;
#[cfg(unix)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, ErrorCode, OptionalExtension as _, Row};

use crate::gateway::gateway_identity::{Receipt, Submission};

/// Mode applied to the database file on unix.
pub const PRIVATE_FILE_MODE: u32 = 0o600;
/// Mask for permission bits.
pub const FILE_MODE_MASK: u32 = 0o777;
/// Largest accepted list limit.
pub const MAX_LIST_LIMIT: i64 = 1_000;

/// The local admission ledger cannot safely apply the requested operation.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionStoreError {
    /// The operation was refused.
    #[error("{0}")]
    Store(String),
    /// A caller argument was invalid (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Database failure.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

fn store_err(msg: &str) -> AdmissionStoreError {
    AdmissionStoreError::Store(msg.to_owned())
}

/// Harness-side knowledge; deliberately distinct from Hermes status values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocalState {
    /// Intent persisted.
    Prepared,
    /// Outcome of submission unknown.
    OutcomeUnknown,
    /// Receipt received from a submission.
    Admitted,
    /// Receipt obtained through authoritative lookup.
    ReceiptObserved,
}

impl LocalState {
    /// The string value Python's `StrEnum` uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Admitted => "admitted",
            Self::ReceiptObserved => "receipt_observed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "outcome_unknown" => Some(Self::OutcomeUnknown),
            "admitted" => Some(Self::Admitted),
            "receipt_observed" => Some(Self::ReceiptObserved),
            _ => None,
        }
    }
}

impl fmt::Display for LocalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Persisted request identity and the latest observed gateway receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionRecord {
    /// Client submission identity.
    pub submission_id: String,
    /// Session identity.
    pub session_id: String,
    /// Gateway admission identity, once known.
    pub admission_id: Option<String>,
    /// Harness-side state.
    pub local_state: LocalState,
    /// Opaque gateway status.
    pub gateway_status: Option<String>,
    /// Execution generation.
    pub execution_generation: Option<u64>,
    /// ISO timestamp of the last update.
    pub updated_at: String,
}

/// SQLite-backed ledger with unique identities and atomic updates.
///
/// The ledger never submits work and never interprets opaque gateway status strings
/// as proof that execution has stopped. Callers must reconcile unknown outcomes
/// using the stable submission ID and the verified Hermes API.
#[derive(Debug)]
pub struct AdmissionStore {
    path: PathBuf,
}

fn is_constraint(err: &rusqlite::Error) -> bool {
    matches!(err, rusqlite::Error::SqliteFailure(e, _) if e.code == ErrorCode::ConstraintViolation)
}

#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn restrict(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn to_record(row: &Row<'_>) -> rusqlite::Result<Result<AdmissionRecord, String>> {
    let state: String = row.get("local_state")?;
    let generation: Option<i64> = row.get("execution_generation")?;
    let Some(local_state) = LocalState::parse(&state) else {
        return Ok(Err(format!("'{state}' is not a valid LocalState")));
    };
    let Ok(execution_generation) = generation.map(u64::try_from).transpose() else {
        return Ok(Err("execution_generation must be nonnegative".to_owned()));
    };
    Ok(Ok(AdmissionRecord {
        submission_id: row.get("submission_id")?,
        session_id: row.get("session_id")?,
        admission_id: row.get("admission_id")?,
        local_state,
        gateway_status: row.get("gateway_status")?,
        execution_generation,
        updated_at: row.get("updated_at")?,
    }))
}

impl AdmissionStore {
    /// Open (creating if needed) the ledger at `path`.
    ///
    /// # Errors
    /// An identity conflicts with the ledger, a receipt is invalid, or filesystem or database
    /// access fails.
    pub fn new(path: &Path) -> Result<Self, AdmissionStoreError> {
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent)?;
        }
        if crate::util::lstat(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(store_err(
                "Admission database path must not be a symbolic link",
            ));
        }
        let store = Self {
            path: path.to_path_buf(),
        };
        store.connect()?.execute(
            "CREATE TABLE IF NOT EXISTS admissions (
                submission_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                admission_id TEXT UNIQUE,
                local_state TEXT NOT NULL,
                gateway_status TEXT,
                execution_generation INTEGER,
                updated_at TEXT NOT NULL
            )",
            [],
        )?;
        restrict(path)?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, AdmissionStoreError> {
        let connection = crate::util::open_sqlite(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
    }

    fn fetch(
        connection: &Connection,
        submission_id: &str,
    ) -> Result<Option<AdmissionRecord>, AdmissionStoreError> {
        let found = connection
            .query_row(
                "SELECT * FROM admissions WHERE submission_id = ?",
                [submission_id],
                to_record,
            )
            .optional()?;
        found
            .map(|parsed| parsed.map_err(AdmissionStoreError::Value))
            .transpose()
    }

    /// Persist intent before network I/O; duplicate IDs are never overwritten.
    ///
    /// # Errors
    /// An identity conflicts with the ledger, a receipt is invalid, or filesystem or database
    /// access fails.
    pub fn prepare(&self, submission: &Submission) -> Result<AdmissionRecord, AdmissionStoreError> {
        if submission.session_id.py_strip().is_empty()
            || submission.submission_id.py_strip().is_empty()
        {
            return Err(AdmissionStoreError::Value(
                "Session and submission IDs must be nonblank".to_owned(),
            ));
        }
        let inserted = self.connect()?.execute(
            "INSERT INTO admissions (
                submission_id, session_id, admission_id, local_state,
                gateway_status, execution_generation, updated_at
            ) VALUES (?, ?, NULL, ?, NULL, NULL, ?)",
            (
                &submission.submission_id,
                &submission.session_id,
                LocalState::Prepared.as_str(),
                crate::util::utc_now_iso(),
            ),
        );
        match inserted {
            Err(err) if is_constraint(&err) => {
                return Err(store_err(
                    "Submission ID already exists; refusing to overwrite its record",
                ));
            }
            Err(err) => return Err(err.into()),
            Ok(_) => {}
        }
        self.get(&submission.submission_id)?
            .ok_or_else(|| store_err("Admission record disappeared after insertion"))
    }

    /// Record uncertainty without deleting any admission identity already known.
    ///
    /// # Errors
    /// An identity conflicts with the ledger, a receipt is invalid, or filesystem or database
    /// access fails.
    pub fn mark_outcome_unknown(
        &self,
        submission_id: &str,
    ) -> Result<AdmissionRecord, AdmissionStoreError> {
        let changed = self.connect()?.execute(
            "UPDATE admissions SET local_state = ?, updated_at = ? WHERE submission_id = ?",
            (
                LocalState::OutcomeUnknown.as_str(),
                crate::util::utc_now_iso(),
                submission_id,
            ),
        )?;
        if changed != 1 {
            return Err(AdmissionStoreError::Store(format!(
                "Unknown submission ID: {submission_id}"
            )));
        }
        self.get(submission_id)?
            .ok_or_else(|| store_err("Admission record disappeared after update"))
    }

    /// Persist a validated receipt and reject changes to an established ID.
    ///
    /// Set `reconciled` only when the receipt was obtained through an authoritative
    /// lookup, not by resubmitting a request whose outcome was lost.
    ///
    /// # Errors
    /// An identity conflicts with the ledger, a receipt is invalid, or filesystem or database
    /// access fails.
    pub fn record_receipt(
        &self,
        submission_id: &str,
        receipt: &Receipt,
        reconciled: bool,
    ) -> Result<AdmissionRecord, AdmissionStoreError> {
        let connection = self.connect()?;
        let current = connection
            .query_row(
                "SELECT admission_id FROM admissions WHERE submission_id = ?",
                [submission_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(current_admission_id) = current else {
            return Err(AdmissionStoreError::Store(format!(
                "Unknown submission ID: {submission_id}"
            )));
        };
        if current_admission_id.is_some_and(|id| id != receipt.admission_id) {
            return Err(store_err(
                "Receipt admission ID conflicts with the durable record",
            ));
        }
        let state = if reconciled {
            LocalState::ReceiptObserved
        } else {
            LocalState::Admitted
        };
        let generation = receipt
            .execution_generation
            .map(i64::try_from)
            .transpose()
            .map_err(|_| AdmissionStoreError::Value("execution_generation too large".to_owned()))?;
        let updated = connection.execute(
            "UPDATE admissions
             SET admission_id = ?, local_state = ?, gateway_status = ?,
                 execution_generation = ?, updated_at = ?
             WHERE submission_id = ?",
            (
                &receipt.admission_id,
                state.as_str(),
                &receipt.status,
                generation,
                crate::util::utc_now_iso(),
                submission_id,
            ),
        );
        match updated {
            Err(err) if is_constraint(&err) => {
                return Err(store_err(
                    "Admission ID is already assigned to another submission",
                ));
            }
            Err(err) => return Err(err.into()),
            Ok(_) => {}
        }
        drop(connection);
        self.get(submission_id)?
            .ok_or_else(|| store_err("Admission record disappeared after receipt update"))
    }

    /// Return one record without changing its state.
    ///
    /// # Errors
    /// An identity conflicts with the ledger, a receipt is invalid, or filesystem or database
    /// access fails.
    pub fn get(&self, submission_id: &str) -> Result<Option<AdmissionRecord>, AdmissionStoreError> {
        Self::fetch(&self.connect()?, submission_id)
    }

    /// List recent records for operator reconciliation, newest update first.
    ///
    /// # Errors
    /// An identity conflicts with the ledger, a receipt is invalid, or filesystem or database
    /// access fails.
    pub fn list_records(&self, limit: i64) -> Result<Vec<AdmissionRecord>, AdmissionStoreError> {
        if !(1..=MAX_LIST_LIMIT).contains(&limit) {
            return Err(AdmissionStoreError::Value(
                "limit must be between 1 and 1000".to_owned(),
            ));
        }
        let connection = self.connect()?;
        let mut statement = connection
            .prepare("SELECT * FROM admissions ORDER BY updated_at DESC, submission_id LIMIT ?")?;
        let rows = statement.query_map([limit], to_record)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row?.map_err(AdmissionStoreError::Value)?);
        }
        Ok(records)
    }
}
