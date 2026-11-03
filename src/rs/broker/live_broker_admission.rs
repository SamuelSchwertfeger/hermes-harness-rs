//! Operator-gated adapter binding socket capabilities to the M3 admission ledger.
//!
//! No gateway transport is constructed here. A trusted operator must inject an
//! independently qualified authenticated RPC; provider calls remain disabled by default.

#[cfg(unix)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, params};
use serde_json::{Map, Value};

use super::live_capability_broker::{BoundRequest, SHA256_HEX_LENGTH, constant_time_eq};
use crate::gateway::admission_store::{AdmissionStore, AdmissionStoreError};
use crate::gateway::gateway_identity::Submission;
use crate::gateway::gateway_live_admission::{
    LiveAdmissionError, LiveAdmissionRequest, submit_once,
};
use crate::gateway::gateway_session_creation::SessionRpc;
use crate::sandbox::live_sandbox_plan::Role;
use crate::services::live_service_controller::{ServiceCommandTransport, ServiceController};
use crate::services::live_service_journal::ServiceJournal;
use crate::supervised::live_execution_binding::{ExecutionBinding, ExecutionBindingStore};
use crate::util::create_private_dir_all;
use crate::util::sha256_hex;

/// No matching, unconsumed human authorization exists.
#[derive(Debug, thiserror::Error)]
pub enum OperatorAuthorizationError {
    /// Authorization refused.
    #[error("{0}")]
    Denied(String),
    /// Database failure (including UNIQUE violations on authorize).
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Admission ledger failure.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// Live admission failure.
    #[error(transparent)]
    Admission(#[from] LiveAdmissionError),
    /// Failure raised by an injected seam (bindings or transport).
    #[error("{0}")]
    Seam(#[source] Box<dyn std::error::Error + Send + Sync>),
}

fn denied(message: &str) -> OperatorAuthorizationError {
    OperatorAuthorizationError::Denied(message.to_owned())
}

/// Explicit authorization, bound to exact request and running service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorGrant {
    /// Task identity.
    pub task_id: String,
    /// `worker` or `reviewer`.
    pub role: Role,
    /// Gateway session identity.
    pub session_id: String,
    /// Submission identity.
    pub submission_id: String,
    /// Service unit.
    pub unit: String,
    /// Lowercase SHA-256 of the prompt text.
    pub text_sha256: String,
}

/// One-time role grants; grants never authorize gateway credentials or commands.
#[derive(Debug)]
pub struct OperatorGrantStore {
    /// Grant database path.
    pub path: PathBuf,
}

#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn restrict(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

impl OperatorGrantStore {
    /// Open (creating if needed) the grant database at `path`.
    ///
    /// # Errors
    /// The database is a symlink, or database creation, initialization, or permission setup fails.
    pub fn new(path: &Path) -> Result<Self, OperatorAuthorizationError> {
        if crate::util::lstat(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(denied("Grant database symlink rejected"));
        }
        if let Some(parent) = path.parent() {
            create_private_dir_all(parent)?;
        }
        let store = Self {
            path: path.to_path_buf(),
        };
        store.connect()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS grants (\
             submission_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, \
             role TEXT NOT NULL, session_id TEXT NOT NULL, unit TEXT NOT NULL, \
             text_sha256 TEXT NOT NULL, consumed INTEGER NOT NULL DEFAULT 0)",
        )?;
        restrict(path)?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, rusqlite::Error> {
        let db = crate::util::open_sqlite(&self.path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        Ok(db)
    }

    /// Only trusted operator control-plane code should ever invoke this method.
    ///
    /// # Errors
    /// Human confirmation or a valid binding is missing, or the grant cannot be inserted (including
    /// duplicates).
    pub fn authorize(
        &self,
        grant: &OperatorGrant,
        human_confirmed: bool,
    ) -> Result<(), OperatorAuthorizationError> {
        if !human_confirmed {
            return Err(denied("Explicit human confirmation required"));
        }
        if grant.task_id.is_empty()
            || grant.session_id.is_empty()
            || grant.submission_id.is_empty()
            || grant.unit.is_empty()
            || grant.text_sha256.chars().count() != SHA256_HEX_LENGTH
        {
            return Err(denied("Invalid authorization binding"));
        }
        self.connect()?.execute(
            "INSERT INTO grants VALUES (?, ?, ?, ?, ?, ?, 0)",
            params![
                grant.submission_id,
                grant.task_id,
                grant.role.as_str(),
                grant.session_id,
                grant.unit,
                grant.text_sha256,
            ],
        )?;
        Ok(())
    }

    /// Atomically fence the authorization before attempting gateway admission.
    ///
    /// # Errors
    /// No matching unconsumed grant exists, or the database update fails.
    pub fn consume(&self, grant: &OperatorGrant) -> Result<(), OperatorAuthorizationError> {
        let changed = self.connect()?.execute(
            "UPDATE grants SET consumed = 1 \
             WHERE submission_id = ? AND task_id = ? AND role = ? \
             AND session_id = ? AND unit = ? AND text_sha256 = ? \
             AND consumed = 0",
            params![
                grant.submission_id,
                grant.task_id,
                grant.role.as_str(),
                grant.session_id,
                grant.unit,
                grant.text_sha256,
            ],
        )?;
        if changed != 1 {
            return Err(denied("No matching unconsumed operator grant"));
        }
        Ok(())
    }
}

/// Seam for `supervised.live_execution_binding.ExecutionBindingStore`, implemented by it.
pub trait ExecutionBindings {
    /// Binding for `task_id` and `role`, if any.
    fn get(
        &self,
        task_id: &str,
        role: &str,
    ) -> Result<Option<ExecutionBinding>, Box<dyn std::error::Error + Send + Sync>>;

    /// Fail unless the journal records the binding's service as running.
    fn require_running(
        &self,
        binding: &ExecutionBinding,
        journal: &ServiceJournal,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

impl ExecutionBindings for ExecutionBindingStore {
    fn get(
        &self,
        task_id: &str,
        role: &str,
    ) -> Result<Option<ExecutionBinding>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self::get(self, task_id, role)?)
    }

    fn require_running(
        &self,
        binding: &ExecutionBinding,
        journal: &ServiceJournal,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self::require_running(self, binding, journal)?)
    }
}

/// Host-only M3 adapter; bind capability, service and grant before RPC.
///
/// If the M3 ledger already contains a submission, never send it again, even after a crash.
pub struct QualifiedAdmissionAdapter<B: ExecutionBindings, T: ServiceCommandTransport> {
    /// One-time grants.
    pub grants: OperatorGrantStore,
    /// Execution bindings.
    pub bindings: B,
    /// Service controller (journal and transport).
    pub services: ServiceController<T>,
    /// M3 admission ledger.
    pub admissions: AdmissionStore,
    /// Authenticated RPC; absent means not qualified.
    pub rpc: Option<Box<dyn SessionRpc>>,
}

impl<B: ExecutionBindings, T: ServiceCommandTransport> std::fmt::Debug
    for QualifiedAdmissionAdapter<B, T>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QualifiedAdmissionAdapter")
            .field("grants_path", &self.grants.path)
            .field("service_journal_path", &self.services.journal.path)
            .field("admissions", &self.admissions)
            .field("rpc_set", &self.rpc.is_some())
            .finish_non_exhaustive()
    }
}

impl<B: ExecutionBindings, T: ServiceCommandTransport> QualifiedAdmissionAdapter<B, T> {
    /// Verify binding, service, prompt and grant, then submit exactly once.
    ///
    /// # Errors
    /// The binding is unauthorized, the service is not running, or ledger or gateway access fails.
    pub fn invoke(
        &mut self,
        request: &BoundRequest,
        text: &str,
    ) -> Result<Map<String, Value>, OperatorAuthorizationError> {
        let Some(rpc) = self.rpc.as_deref_mut() else {
            return Err(denied("Authenticated Hermes RPC not qualified"));
        };
        let binding = self
            .bindings
            .get(&request.task_id, request.role.as_str())
            .map_err(OperatorAuthorizationError::Seam)?
            .filter(|b| {
                b.session_id == request.session_id && b.submission_id == request.submission_id
            })
            .ok_or_else(|| denied("Missing service/submission binding"))?;
        self.bindings
            .require_running(&binding, &self.services.journal)
            .map_err(OperatorAuthorizationError::Seam)?;
        if !self
            .services
            .transport
            .is_active(&binding.unit)
            .map_err(OperatorAuthorizationError::Seam)?
        {
            return Err(denied("Stale or stopped service"));
        }
        let digest = sha256_hex(text.as_bytes());
        if !constant_time_eq(digest.as_bytes(), request.text_sha256.as_bytes()) {
            return Err(denied("Prompt mismatch"));
        }
        if self.admissions.get(&request.submission_id)?.is_some() {
            return Err(denied("M3 submission already recorded or uncertain"));
        }
        self.grants.consume(&OperatorGrant {
            task_id: request.task_id.clone(),
            role: request.role,
            session_id: request.session_id.clone(),
            submission_id: request.submission_id.clone(),
            unit: binding.unit,
            text_sha256: digest,
        })?;
        let result = submit_once(
            &self.admissions,
            &LiveAdmissionRequest {
                submission: Submission::new(&request.session_id, &request.submission_id),
                text: text.to_owned(),
            },
            rpc,
        )?;
        let mut out = Map::new();
        out.insert(
            "admission_id".to_owned(),
            Value::String(result.admission_id),
        );
        Ok(out)
    }
}
