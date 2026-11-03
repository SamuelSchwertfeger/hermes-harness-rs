//! Offline durable reservation and read-only task recovery.
//!
//! A dispatch fence is deliberately irreversible: even a crash before invoking an
//! agent requires manual resolution. Local records are not authenticated evidence.

use crate::util::PathLexical;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::delegation::{
    DelegationError, create_new_private, create_private_dir, digest, validate,
};
use crate::git::worktree_delegation::TaskWorkspace;
use crate::git::worktrees::{WorktreeError, git, resolve_lenient, resolve_strict};
use crate::util::{canonical_json, canonical_json_value, sha256_hex};

crate::util::wire_enum! {
    /// Recovery classification of a task, derived read-only from evidence.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum RecoveryState {
        /// Reservation exists, nothing else.
        Reserved => "reserved",
        /// Worktree preparation started; outcome uncertain.
        Preparing => "preparing_uncertain",
        /// Worktree prepared and bound.
        Prepared => "prepared",
        /// Worker dispatched without complete evidence.
        DispatchUncertain => "dispatch_uncertain",
        /// Worker output and audit recorded.
        WorkerRecorded => "worker_recorded",
        /// Evidence inconsistent; a human must resolve.
        ManualIntervention => "manual_intervention",
    }

    /// The Python enum value.
    as_str;
}

/// Durable identity and baseline recorded before any Git mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    /// Task identifier.
    pub task_id: String,
    /// Digest of the task brief.
    pub brief_digest: String,
    /// Source repository path.
    pub source_repo: String,
    /// Workspace path.
    pub workspace: String,
    /// Baseline commit hash.
    pub baseline: String,
    /// Branch name (`harness/<task-id>`).
    pub branch: String,
}

/// Errors of reservation, fencing and recovery.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReservationError {
    /// Workflow rule violation.
    #[error(transparent)]
    Delegation(#[from] DelegationError),
    /// Git or ownership failure.
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
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

fn fail<T>(msg: &str) -> Result<T, ReservationError> {
    Err(DelegationError::Message(msg.to_owned()).into())
}

fn json_line<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, crate::util::JsonError> {
    let mut bytes = canonical_json(value)?.into_bytes();
    bytes.push(b'\n');
    Ok(bytes)
}

fn atomic_new<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), ReservationError> {
    let stream = create_new_private(path)?;
    write_new_bytes(path, stream, &json_line(value)?)
}

fn atomic_new_value(path: &Path, value: &Value) -> Result<(), ReservationError> {
    let stream = create_new_private(path)?;
    let mut payload = canonical_json_value(value)?;
    payload.push('\n');
    write_new_bytes(path, stream, payload.as_bytes())
}

fn write_new_bytes(path: &Path, mut stream: File, bytes: &[u8]) -> Result<(), ReservationError> {
    stream.write_all(bytes)?;
    stream.flush()?;
    stream.sync_all()?;
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        #[cfg(not(unix))]
        let _ = parent;
    }
    Ok(())
}

/// Interprocess lock guard protecting reservation creation and stage fences.
#[derive(Debug)]
pub struct TaskLock {
    file: File,
    #[cfg(not(unix))]
    key: PathBuf,
}

impl Drop for TaskLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
        }
        #[cfg(not(unix))]
        {
            let _ = &self.file;
            crate::util::process_lock::release(&self.key);
        }
    }
}

fn ensure_lock_dir(locks: &Path) -> Result<(), ReservationError> {
    if locks.is_symlink_lexical() {
        return fail("Symlink lock directory");
    }
    if let Err(err) = create_private_dir(locks)
        && !(err.kind() == std::io::ErrorKind::AlreadyExists && locks.is_dir())
    {
        return Err(err.into());
    }
    if locks.is_symlink_lexical() {
        return fail("Symlink lock directory");
    }
    Ok(())
}

fn open_lock_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let nofollow = i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or_default();
        options.mode(0o600).custom_flags(nofollow);
    }
    options.open(path)
}

/// Take the exclusive lock of one task; released when the guard drops.
///
/// # Errors
/// Reservation identity, digest, or state is invalid, or reservation files cannot be accessed.
pub fn task_lock(root: &Path, task_id: &str) -> Result<TaskLock, ReservationError> {
    if root.is_symlink_lexical() || !root.is_dir() {
        return fail("State root must be a real directory");
    }
    let locks = root.join(".reservation-locks");
    ensure_lock_dir(&locks)?;
    let path = locks.join(format!("{task_id}.lock"));
    let file = open_lock_file(&path)?;
    #[cfg(unix)]
    {
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(std::io::Error::from)?;
        Ok(TaskLock { file })
    }
    #[cfg(not(unix))]
    {
        crate::util::process_lock::acquire(&path);
        Ok(TaskLock { file, key: path })
    }
}

fn safe_workspace(
    task: &TaskWorkspace,
    repo: &Path,
) -> Result<(PathBuf, PathBuf), ReservationError> {
    validate(&task.brief)?;
    let root = resolve_strict(&task.state_root)?;
    let source = resolve_strict(repo)?;
    let target = std::path::absolute(&task.workspace)?;
    if target.is_symlink_lexical() || resolve_lenient(&target) != target {
        return fail("Workspace path includes symlink or noncanonical components");
    }
    if target.starts_with(&source) || source.starts_with(&target) {
        return fail("Workspace and source repository overlap");
    }
    if target.starts_with(&root) || root.starts_with(&target) {
        return fail("Workspace and evidence root overlap");
    }
    if Path::new(&git(&source, &["rev-parse", "--show-toplevel"])?) != source {
        return fail("Source must be Git root");
    }
    if !git(&source, &["status", "--porcelain"])?.is_empty() {
        return fail("Source must be clean");
    }
    Ok((source, target))
}

/// Persist identity and baseline BEFORE performing Git worktree mutations.
///
/// # Errors
/// Reservation identity, digest, or state is invalid, or reservation files cannot be accessed.
pub fn reserve(task: &TaskWorkspace, repo: &Path) -> Result<Reservation, ReservationError> {
    let (source, target) = safe_workspace(task, repo)?;
    let brief = &task.brief;
    let _lock = task_lock(&task.state_root, &brief.task_id)?;
    let task_dir = task.state_root.join(&brief.task_id);
    if target.exists() || target.is_symlink_lexical() {
        return Err(WorktreeError::Workspace("Workspace already exists".to_owned()).into());
    }
    if task_dir.exists() || task_dir.is_symlink_lexical() {
        return fail("Task ID already reserved");
    }
    let baseline = git(&source, &["rev-parse", "HEAD"])?;
    let record = Reservation {
        task_id: brief.task_id.clone(),
        brief_digest: digest(brief)?,
        source_repo: crate::util::path_str(&source)?.to_owned(),
        workspace: crate::util::path_str(&target)?.to_owned(),
        baseline,
        branch: format!("harness/{}", brief.task_id),
    };
    create_private_dir(&task_dir)?;
    atomic_new(&task_dir.join("reservation.json"), &record)?;
    Ok(record)
}

/// Load and verify the reservation of a task.
///
/// # Errors
/// Reservation identity, digest, or state is invalid, or reservation files cannot be accessed.
pub fn load(task: &TaskWorkspace) -> Result<Reservation, ReservationError> {
    let path = task
        .state_root
        .join(&task.brief.task_id)
        .join("reservation.json");
    if path.is_symlink_lexical() || path.parent().is_some_and(PathLexical::is_symlink_lexical) {
        return fail("Symlink reservation evidence");
    }
    let record: Reservation = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .map_or_else(|| fail("Reservation missing or malformed"), Ok)?;
    let workspace = std::path::absolute(&task.workspace)
        .and_then(|p| crate::util::path_str(&p).map(str::to_owned));
    let expected_identity = record.task_id == task.brief.task_id
        && record.brief_digest == digest(&task.brief)?
        && workspace.is_ok_and(|text| record.workspace == text);
    let expected_branch = record.branch == format!("harness/{}", task.brief.task_id);
    let baseline_valid = record.baseline.len() == 40
        && record
            .baseline
            .chars()
            .all(|ch| matches!(ch, '0'..='9' | 'a'..='f'));
    if !expected_identity || !expected_branch || !baseline_valid {
        return fail("Reservation identity or digest mismatch");
    }
    Ok(record)
}

/// Record an irreversible side-effect boundary, under the task lock.
///
/// # Errors
/// Reservation identity, digest, or state is invalid, or reservation files cannot be accessed.
pub fn fence(task: &TaskWorkspace, stage: &str) -> Result<(), ReservationError> {
    if !STAGE_NAMES.contains(&stage) {
        return fail("Unknown reservation stage");
    }
    load(task)?;
    let path = task
        .state_root
        .join(&task.brief.task_id)
        .join(format!("{stage}.json"));
    atomic_new_value(&path, &serde_json::json!({ "stage": stage }))
}

/// Names of the fence markers, in lifecycle order.
pub const STAGE_NAMES: [&str; 4] = ["preparing", "prepared", "dispatch", "review_dispatch"];

#[derive(Debug, Clone, Copy)]
struct Stages {
    preparing: bool,
    prepared: bool,
    dispatch: bool,
}

/// Stage presence; malformed or out-of-order markers fail closed (`None`).
fn stage_evidence(directory: &Path) -> Result<Option<Stages>, ReservationError> {
    let mut present = [false; 4];
    for (slot, name) in present.iter_mut().zip(STAGE_NAMES) {
        let marker = directory.join(format!("{name}.json"));
        if marker.is_symlink_lexical() {
            return Ok(None);
        }
        if marker.exists() {
            let value: Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
            if value != serde_json::json!({ "stage": name }) {
                return Ok(None);
            }
        }
        *slot = marker.exists();
    }
    let [preparing, prepared, dispatch, review_dispatch] = present;
    if (prepared && !preparing) || (dispatch && !prepared) || (review_dispatch && !dispatch) {
        return Ok(None);
    }
    Ok(Some(Stages {
        preparing,
        prepared,
        dispatch,
    }))
}

fn worker_state(directory: &Path, worker_id: &str) -> Result<RecoveryState, ReservationError> {
    let worker = directory.join("worker.json");
    let audit = directory.join("worktree-audit.json");
    if worker.is_symlink_lexical() || audit.is_symlink_lexical() {
        return Ok(RecoveryState::ManualIntervention);
    }
    if !worker.is_file() || !audit.is_file() {
        return Ok(RecoveryState::DispatchUncertain);
    }
    let worker_data: Value = serde_json::from_slice(&std::fs::read(&worker)?)?;
    let audit_data: Value = serde_json::from_slice(&std::fs::read(&audit)?)?;
    if !worker_data.is_object() || !audit_data.is_object() {
        return Ok(RecoveryState::ManualIntervention);
    }
    let valid_worker = worker_data.get("agent_id").and_then(Value::as_str) == Some(worker_id)
        && worker_data.get("text").is_some_and(Value::is_string);
    let valid_audit = audit_data.get("audit").is_some_and(Value::is_object)
        && audit_data
            .get("content_sha256")
            .is_some_and(Value::is_string);
    Ok(if valid_worker && valid_audit {
        RecoveryState::WorkerRecorded
    } else {
        RecoveryState::ManualIntervention
    })
}

fn prepared_state(
    directory: &Path,
    record: &Reservation,
) -> Result<RecoveryState, ReservationError> {
    let paths = [
        directory.join("brief.json"),
        directory.join("worktree-binding.json"),
    ];
    if paths
        .iter()
        .any(|path| path.is_symlink_lexical() || !path.is_file())
    {
        return Ok(RecoveryState::ManualIntervention);
    }
    let workspace = Path::new(&record.workspace);
    if workspace.is_symlink_lexical() || !workspace.is_dir() {
        return Ok(RecoveryState::ManualIntervention);
    }
    let baseline_matches = git(workspace, &["rev-parse", "HEAD"])? == record.baseline;
    let branch_matches = git(workspace, &["symbolic-ref", "--short", "HEAD"])? == record.branch;
    Ok(if baseline_matches && branch_matches {
        RecoveryState::Prepared
    } else {
        RecoveryState::ManualIntervention
    })
}

fn reconcile_state(
    task: &TaskWorkspace,
    record: &Reservation,
    present: Stages,
) -> Result<RecoveryState, ReservationError> {
    let directory = task.state_root.join(&task.brief.task_id);
    if present.dispatch {
        return worker_state(&directory, &task.brief.worker_id);
    }
    if present.prepared {
        return prepared_state(&directory, record);
    }
    if present.preparing {
        return Ok(RecoveryState::Preparing);
    }
    if Path::new(&record.workspace).exists() {
        return Ok(RecoveryState::ManualIntervention);
    }
    Ok(RecoveryState::Reserved)
}

/// Read-only inspection, without replaying Git or agent side effects.
///
/// # Errors
/// Reservation identity, digest, or state is invalid, or reservation files cannot be accessed.
pub fn reconcile(task: &TaskWorkspace) -> Result<RecoveryState, ReservationError> {
    let record = load(task)?;
    let directory = task.state_root.join(&task.brief.task_id);
    let outcome = stage_evidence(&directory).and_then(|present| {
        present.map_or(Ok(RecoveryState::ManualIntervention), |stages| {
            reconcile_state(task, &record, stages)
        })
    });
    Ok(outcome.unwrap_or(RecoveryState::ManualIntervention))
}

/// Local checksum for display; not a trusted signature.
///
/// # Errors
/// Reservation data cannot be encoded by the Python-compatible JSON encoder.
pub fn reservation_digest(record: &Reservation) -> Result<String, crate::util::JsonError> {
    Ok(sha256_hex(&json_line(record)?))
}
