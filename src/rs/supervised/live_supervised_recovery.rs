//! Durable, observation-only recovery for the supervised execution pipeline.
//!
//! Never submits gateway work, starts a service, retries an uncertain request, or
//! approves a decision. An operator must separately authorize each live role.

use crate::util::PathLexical as _;
pub(crate) use crate::util::ensure_private_leaf_dir as create_private_dir;
use crate::util::is_py_blank;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::agents::supervised_review_handoff::{
    ReviewerHandoff, ReviewerHandoffError, verify_reviewer_handoff,
};
use crate::gateway::admission_store::{AdmissionStore, AdmissionStoreError};
use crate::git::worktree_delegation::TaskWorkspace;
use crate::util::{JsonError, json_digest, json_digest_value, sha256_hex};

/// A persisted supervised operation cannot safely advance.
#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    /// A recovery rule was violated.
    #[error("{0}")]
    Refused(String),
    /// A persisted phase name is unknown (Python `ValueError`).
    #[error("{0:?} is not a valid Phase")]
    InvalidPhase(String),
    /// The admission ledger failed.
    #[error(transparent)]
    Store(#[from] AdmissionStoreError),
    /// The handoff verifier failed; propagated unchanged.
    #[error(transparent)]
    Handoff(#[from] ReviewerHandoffError),
    /// SQLite failure.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Malformed decision JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

fn refuse<T>(msg: &str) -> Result<T, RecoveryError> {
    Err(RecoveryError::Refused(msg.to_owned()))
}

crate::util::wire_enum! {
    /// Monotonic evidence checkpoints; never an execution instruction.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
    pub enum Phase {
        /// Nothing observed yet.
        Planned => "planned",
        /// Worker gateway admission is durable.
        WorkerAdmitted => "worker_admitted",
        /// Worker Git audit and handoff verified.
        WorkerAudited => "worker_audited",
        /// Reviewer gateway admission is durable.
        ReviewerAdmitted => "reviewer_admitted",
        /// A pending human decision request exists.
        PendingHuman => "pending_human",
        /// Operation frozen.
        Halted => "halted",
    }

    /// The Python string value of the phase.
    as_str;
    from_str(value) -> RecoveryError { RecoveryError::InvalidPhase(value.to_owned()) }
}

impl Phase {
    /// Parse a persisted phase name.
    ///
    /// # Errors
    /// The persisted name is not a recognized recovery phase.
    pub fn parse(value: &str) -> Result<Self, RecoveryError> {
        value.parse()
    }
}

/// Longest accepted halt reason in characters.
pub const MAX_HALT_REASON_LENGTH: usize = 256;
/// Length of a hex-encoded SHA-256 digest.
pub const SHA256_HEX_LENGTH: usize = 64;

/// Evidence phases in their only legal order.
pub const PHASE_ORDER: [Phase; 5] = [
    Phase::Planned,
    Phase::WorkerAdmitted,
    Phase::WorkerAudited,
    Phase::ReviewerAdmitted,
    Phase::PendingHuman,
];

/// Stable task and gateway identities supplied by the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryIdentity {
    /// Task identity.
    pub task_id: String,
    /// Worker submission identity.
    pub worker_submission_id: String,
    /// Reviewer submission identity.
    pub reviewer_submission_id: String,
}

impl RecoveryIdentity {
    /// Validate and build an identity (Python `__post_init__`).
    ///
    /// # Errors
    /// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
    pub fn new(
        task_id: &str,
        worker_submission_id: &str,
        reviewer_submission_id: &str,
    ) -> Result<Self, RecoveryError> {
        let identity = Self {
            task_id: task_id.to_owned(),
            worker_submission_id: worker_submission_id.to_owned(),
            reviewer_submission_id: reviewer_submission_id.to_owned(),
        };
        identity.validate()?;
        Ok(identity)
    }

    fn validate(&self) -> Result<(), RecoveryError> {
        let ids = [
            &self.task_id,
            &self.worker_submission_id,
            &self.reviewer_submission_id,
        ];
        if ids.iter().any(|value| is_py_blank(value)) {
            return refuse("Nonblank identity fields required");
        }
        if self.worker_submission_id == self.reviewer_submission_id {
            return refuse("Worker and reviewer must be independent submissions");
        }
        Ok(())
    }
}

/// A durable statement about evidence already inspected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCheckpoint {
    /// Current phase.
    pub phase: Phase,
    /// Digest of the evidence that reached this phase.
    pub evidence_sha256: String,
    /// Why the operation was halted, if it was.
    pub halted_reason: Option<String>,
}

/// Single-writer-independent SQLite CAS journal; no automatic execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryLedger {
    /// Database file path.
    pub path: PathBuf,
}

/// Restrict a freshly created database file to its owner.
#[cfg(unix)]
pub(crate) fn restrict_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

impl RecoveryLedger {
    /// Open or create the ledger (parent dir mode 0700, file mode 0600 on unix).
    ///
    /// # Errors
    /// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
    pub fn new(path: &Path) -> Result<Self, RecoveryError> {
        if path.is_symlink_lexical() {
            return refuse("Recovery ledger symlink forbidden");
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            create_private_dir(parent)?;
        }
        let ledger = Self {
            path: path.to_path_buf(),
        };
        ledger.connect()?.execute(
            "CREATE TABLE IF NOT EXISTS recovery (\
             task_id TEXT PRIMARY KEY, worker_id TEXT NOT NULL, \
             reviewer_id TEXT NOT NULL, phase TEXT NOT NULL, \
             evidence_sha256 TEXT NOT NULL, halted_reason TEXT)",
            [],
        )?;
        #[cfg(unix)]
        restrict_file(path)?;
        Ok(ledger)
    }

    fn connect(&self) -> Result<Connection, RecoveryError> {
        let connection = crate::util::open_sqlite(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
    }

    /// Idempotent only for identical identities; no state reset.
    ///
    /// # Errors
    /// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
    pub fn initialize(
        &self,
        identity: &RecoveryIdentity,
    ) -> Result<RecoveryCheckpoint, RecoveryError> {
        identity.validate()?;
        self.connect()?.execute(
            "INSERT OR IGNORE INTO recovery VALUES (?, ?, ?, ?, ?, NULL)",
            params![
                identity.task_id,
                identity.worker_submission_id,
                identity.reviewer_submission_id,
                Phase::Planned.as_str(),
                "",
            ],
        )?;
        self.read(identity)
    }

    /// Read the persisted checkpoint if the identity matches it exactly.
    ///
    /// # Errors
    /// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
    pub fn read(&self, identity: &RecoveryIdentity) -> Result<RecoveryCheckpoint, RecoveryError> {
        identity.validate()?;
        let row = self
            .connect()?
            .query_row(
                "SELECT worker_id, reviewer_id, phase, evidence_sha256, halted_reason \
                 FROM recovery WHERE task_id = ?",
                params![identity.task_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((worker, reviewer, phase, evidence_sha256, halted_reason)) = row else {
            return refuse("Recovery identity missing or conflicts with persisted task");
        };
        if worker != identity.worker_submission_id || reviewer != identity.reviewer_submission_id {
            return refuse("Recovery identity missing or conflicts with persisted task");
        }
        Ok(RecoveryCheckpoint {
            phase: Phase::parse(&phase)?,
            evidence_sha256,
            halted_reason,
        })
    }

    /// Commit exactly the next evidence phase; reject gaps and stale writers.
    ///
    /// # Errors
    /// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
    pub fn advance(
        &self,
        identity: &RecoveryIdentity,
        transition: (Phase, Phase),
        evidence_sha256: &str,
    ) -> Result<RecoveryCheckpoint, RecoveryError> {
        let (expected, target) = transition;
        let current = self.read(identity)?;
        if current.phase != expected {
            return refuse("Stale checkpoint or halted operation; inspect before proceeding");
        }
        let position = |phase: Phase| PHASE_ORDER.iter().position(|item| *item == phase);
        let (Some(from), Some(to)) = (position(expected), position(target)) else {
            return refuse("Halted operations cannot resume automatically");
        };
        if to != from + 1 {
            return refuse("Recovery evidence cannot skip phases");
        }
        if evidence_sha256.chars().count() != SHA256_HEX_LENGTH {
            return refuse("Expected SHA256 evidence digest");
        }
        let changed = self.connect()?.execute(
            "UPDATE recovery SET phase=?, evidence_sha256=? \
             WHERE task_id=? AND phase=? AND evidence_sha256=? AND halted_reason IS NULL",
            params![
                target.as_str(),
                evidence_sha256,
                identity.task_id,
                expected.as_str(),
                current.evidence_sha256,
            ],
        )?;
        if changed != 1 {
            return refuse("Concurrent checkpoint update rejected");
        }
        self.read(identity)
    }

    /// Durably freeze an operation on denial, cancellation or uncertainty.
    ///
    /// # Errors
    /// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
    pub fn halt(
        &self,
        identity: &RecoveryIdentity,
        reason: &str,
    ) -> Result<RecoveryCheckpoint, RecoveryError> {
        if is_py_blank(reason) || reason.chars().count() > MAX_HALT_REASON_LENGTH {
            return refuse("Bounded nonempty halt reason required");
        }
        self.read(identity)?;
        self.connect()?.execute(
            "UPDATE recovery SET phase=?, halted_reason=? WHERE task_id=? AND phase != ?",
            params![
                Phase::Halted.as_str(),
                reason,
                identity.task_id,
                Phase::Halted.as_str(),
            ],
        )?;
        self.read(identity)
    }
}

/// Digest deterministic non-secret evidence metadata, not prompt contents.
///
/// # Errors
/// Evidence cannot be encoded by the Python-compatible JSON encoder.
pub fn evidence_digest<T: Serialize + ?Sized>(document: &T) -> Result<String, RecoveryError> {
    Ok(json_digest(document)?)
}

fn evidence_digest_value(document: &Value) -> Result<String, RecoveryError> {
    Ok(json_digest_value(document)?)
}

/// Read authoritative admission records without a gateway call or replay.
///
/// # Errors
/// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
pub fn inspect_admissions(
    identity: &RecoveryIdentity,
    store: &AdmissionStore,
) -> Result<(bool, bool), RecoveryError> {
    let worker = store.get(&identity.worker_submission_id)?;
    let reviewer = store.get(&identity.reviewer_submission_id)?;
    let worker_known = worker.as_ref().is_some_and(|record| {
        record
            .admission_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
    });
    if reviewer.is_some() && !worker_known {
        return refuse("Reviewer admission precedes known worker admission");
    }
    Ok((
        worker
            .as_ref()
            .is_some_and(|record| record.admission_id.is_some()),
        reviewer
            .as_ref()
            .is_some_and(|record| record.admission_id.is_some()),
    ))
}

/// Validate current Git audit before allowing human-gated reviewer admission.
///
/// # Errors
/// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
pub fn inspect_audit(
    task: &TaskWorkspace,
    handoff: &ReviewerHandoff,
) -> Result<String, RecoveryError> {
    verify_reviewer_handoff(task, handoff)?;
    evidence_digest_value(&json!({
        "task_id": handoff.task_id,
        "worker_submission_id": handoff.worker_submission_id,
        "worker_admission_id": handoff.worker_admission_id,
        "audit_sha256": handoff.audit_sha256,
    }))
}

/// Verify one-shot decision evidence is still pending and identity-bound.
///
/// # Errors
/// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
pub fn inspect_pending_decision(
    path: &Path,
    identity: &RecoveryIdentity,
) -> Result<String, RecoveryError> {
    if path.is_symlink_lexical() || !path.is_file() {
        return refuse("Human decision request missing or unsafe");
    }
    let raw = fs::read(path)?;
    let document: Value = serde_json::from_slice(&raw)?;
    let Value::Object(document) = document else {
        return refuse("Invalid human decision request");
    };
    let field = |key: &str| document.get(key).and_then(Value::as_str);
    let matches_identity = field("task_id") == Some(identity.task_id.as_str())
        && field("worker_submission_id") == Some(identity.worker_submission_id.as_str())
        && field("reviewer_submission_id") == Some(identity.reviewer_submission_id.as_str());
    let is_pending = document.get("approved") == Some(&Value::Bool(false))
        && field("human_decision") == Some("pending");
    if !matches_identity || !is_pending {
        return refuse("Decision record not pending or identity mismatch");
    }
    Ok(sha256_hex(&raw))
}

/// Record existing worker admission; never invokes a provider or gateway.
///
/// # Errors
/// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
pub fn checkpoint_worker_admission(
    ledger: &RecoveryLedger,
    identity: &RecoveryIdentity,
    store: &AdmissionStore,
) -> Result<RecoveryCheckpoint, RecoveryError> {
    let (worker_known, _) = inspect_admissions(identity, store)?;
    if !worker_known {
        return refuse("Worker gateway admission is not durably established");
    }
    let Some(worker) = store.get(&identity.worker_submission_id)? else {
        return refuse("Worker admission disappeared");
    };
    ledger.advance(
        identity,
        (Phase::Planned, Phase::WorkerAdmitted),
        &evidence_digest_value(&json!({
            "submission_id": worker.submission_id,
            "session_id": worker.session_id,
            "admission_id": worker.admission_id,
        }))?,
    )
}

/// Record independently verified Git audit and immutable reviewer handoff.
///
/// # Errors
/// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
pub fn checkpoint_worker_audit(
    ledger: &RecoveryLedger,
    identity: &RecoveryIdentity,
    audit: (&TaskWorkspace, &ReviewerHandoff),
) -> Result<RecoveryCheckpoint, RecoveryError> {
    let (task, handoff) = audit;
    if task.brief.task_id != identity.task_id
        || handoff.worker_submission_id != identity.worker_submission_id
    {
        return refuse("Worktree and handoff identity do not match recovery task");
    }
    let digest = inspect_audit(task, handoff)?;
    ledger.advance(
        identity,
        (Phase::WorkerAdmitted, Phase::WorkerAudited),
        &digest,
    )
}

/// Record existing reviewer admission with an independent gateway session.
///
/// # Errors
/// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
pub fn checkpoint_reviewer_admission(
    ledger: &RecoveryLedger,
    identity: &RecoveryIdentity,
    store: &AdmissionStore,
) -> Result<RecoveryCheckpoint, RecoveryError> {
    let (worker_known, reviewer_known) = inspect_admissions(identity, store)?;
    if !worker_known || !reviewer_known {
        return refuse("Both gateway admissions must be durably established");
    }
    let worker = store.get(&identity.worker_submission_id)?;
    let reviewer = store.get(&identity.reviewer_submission_id)?;
    let (Some(worker), Some(reviewer)) = (worker, reviewer) else {
        return refuse("Reviewer admission must be independent of worker");
    };
    if worker.session_id == reviewer.session_id || worker.admission_id == reviewer.admission_id {
        return refuse("Reviewer admission must be independent of worker");
    }
    ledger.advance(
        identity,
        (Phase::WorkerAudited, Phase::ReviewerAdmitted),
        &evidence_digest_value(&json!({
            "submission_id": reviewer.submission_id,
            "session_id": reviewer.session_id,
            "admission_id": reviewer.admission_id,
        }))?,
    )
}

/// Only verify an already-written pending decision; never produce approval.
///
/// # Errors
/// Recovery identity, phase, or evidence is inconsistent, or evidence or ledger access fails.
pub fn checkpoint_pending_human(
    ledger: &RecoveryLedger,
    identity: &RecoveryIdentity,
    request_path: &Path,
) -> Result<RecoveryCheckpoint, RecoveryError> {
    let digest = inspect_pending_decision(request_path, identity)?;
    ledger.advance(
        identity,
        (Phase::ReviewerAdmitted, Phase::PendingHuman),
        &digest,
    )
}
