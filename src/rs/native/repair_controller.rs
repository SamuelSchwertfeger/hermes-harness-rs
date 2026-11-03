//! Append-only, bounded Hermes repair coordination (never executes repairs).
//!
//! The coordinator reserves at most three attempts against one failed functional receipt.
//! It does not grant inference, worktree writes, code execution or merges.

use crate::util::PathLexical as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::lifecycle::load;
use crate::deployment::gpu::write_once_value as write_once;
use crate::multiagent::runtime::WorkflowError;
use crate::util::is_slug_id;
use crate::util::{py_path_str, resolve_lenient, resolve_strict, sha256_hex};

const MAX_ATTEMPTS: usize = 3;
const OUTCOMES: [&str; 4] = ["rejected", "failed", "passed", "abandoned"];
const MAX_REPLACEMENT_BYTES: usize = 48 * 1024;

/// Most files one repair attempt may replace.
pub const MAX_REPAIR_FILES: usize = 16;

pub(crate) fn require(condition: bool, message: &str) -> Result<(), WorkflowError> {
    if condition {
        Ok(())
    } else {
        Err(WorkflowError::msg(message))
    }
}

fn state_root(directory: &Path) -> Result<PathBuf, WorkflowError> {
    require(
        directory.is_dir() && !directory.is_symlink_lexical(),
        "State directory must exist",
    )?;
    Ok(resolve_strict(directory)?)
}

fn read(path: &Path) -> Result<Map<String, Value>, WorkflowError> {
    Ok(load(path)?.0)
}

fn flag(record: &Map<String, Value>, name: &str, expected: bool) -> bool {
    record.get(name) == Some(&Value::Bool(expected))
}

fn text<'a>(record: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    record.get(name).and_then(Value::as_str)
}

pub(crate) fn text_of<'a>(report: &'a Value, name: &str) -> Option<&'a str> {
    report.get(name).and_then(Value::as_str)
}

/// `Path.resolve()`: canonical when the entry exists, lexical otherwise.
pub(crate) fn resolved(path: &Path) -> PathBuf {
    resolve_strict(path).unwrap_or_else(|_| resolve_lenient(path))
}

fn exists(path: &Path) -> bool {
    path.exists() || path.is_symlink_lexical()
}

/// Missing key, where Python would raise `KeyError`.
fn required<'a>(record: &'a Map<String, Value>, name: &str) -> Result<&'a Value, WorkflowError> {
    record
        .get(name)
        .ok_or_else(|| WorkflowError::Message(format!("'{name}'")))
}

fn with_evidence(record: Value, path: &Path, digest: &str) -> Value {
    let mut result = record;
    if let Value::Object(map) = &mut result {
        map.insert("evidence".to_owned(), json!(py_path_str(path)));
        map.insert("evidence_sha256".to_owned(), json!(digest));
    }
    result
}

/// Open a repair sequence from a qualifying failed sandbox test receipt.
///
/// # Errors
/// Missing authorization, a receipt that is not a qualifying, checkpoint-bound failure, or an
/// already opened sequence (an `AlreadyExists` I/O error).
pub fn open_repair(
    failure: &Path,
    checkpoint: &Path,
    state: &Path,
    authorized: bool,
) -> Result<Value, WorkflowError> {
    require(
        authorized,
        "Explicit authorization to open repair tracking required",
    )?;
    let root = state_root(state)?;
    let (failed, failed_hash) = load(failure)?;
    let (original, checkpoint_hash) = load(checkpoint)?;
    require(
        text(&original, "mode") == Some("integrity-checkpoint"),
        "Invalid checkpoint",
    )?;
    require(
        text(&failed, "mode") == Some("sandboxed-functional-verification"),
        "Invalid failure receipt",
    )?;
    require(
        flag(&failed, "functional_verified", false),
        "Functional verification did not fail",
    )?;
    require(
        flag(&failed, "tests_executed", true),
        "Execution unconfirmed; refuse automatic repair",
    )?;
    require(
        flag(&failed, "timed_out", false),
        "Timeout requires operator investigation",
    )?;
    require(
        flag(&failed, "sandbox_started", true),
        "Sandbox failure is not repairable test failure",
    )?;
    require(
        flag(&failed, "human_decision_pending", true),
        "Receipt not awaiting a decision",
    )?;
    require(
        text(&failed, "checkpoint_sha256") == Some(checkpoint_hash.as_str()),
        "Checkpoint digest mismatch",
    )?;
    for field in ["task_id", "baseline", "workspace"] {
        require(
            failed.get(field) == original.get(field),
            &format!("Repair identity mismatch: {field}"),
        )?;
    }
    let task_id = match text(&original, "task_id") {
        Some(id) if is_slug_id(id) => id,
        _ => return Err(WorkflowError::msg("Invalid task ID")),
    };
    require(
        original.get("max_repair_attempts") == Some(&json!(MAX_ATTEMPTS)),
        "Unexpected attempt budget",
    )?;
    let record = json!({
        "mode": "repair-sequence",
        "task_id": task_id,
        "baseline": required(&original, "baseline")?,
        "workspace": required(&original, "workspace")?,
        "checkpoint_sha256": checkpoint_hash,
        "failure_sha256": failed_hash,
        "attempt_limit": MAX_ATTEMPTS,
        "phase": "await-attempt-reservation",
        "automatic_execution_authorized": false,
        "human_decision_pending": true,
    });
    let destination = root.join(format!("{task_id}-repair-open.json"));
    let digest = write_once(&destination, &record)?;
    Ok(with_evidence(record, &destination, &digest))
}

/// State root and the opened sequence record for `task_id`.
pub(crate) fn sequence(
    state: &Path,
    task_id: &str,
) -> Result<(PathBuf, Map<String, Value>), WorkflowError> {
    require(is_slug_id(task_id), "Invalid task identifier")?;
    let root = state_root(state)?;
    let record = read(&root.join(format!("{task_id}-repair-open.json")))?;
    require(
        text(&record, "mode") == Some("repair-sequence")
            && text(&record, "task_id") == Some(task_id),
        "Invalid repair sequence",
    )?;
    Ok((root, record))
}

fn attempt_path(root: &Path, task_id: &str, number: usize) -> PathBuf {
    root.join(format!("{task_id}-repair-{number}.json"))
}

fn disposition_path(root: &Path, task_id: &str, number: usize) -> PathBuf {
    root.join(format!("{task_id}-repair-{number}-disposition.json"))
}

fn read_attempts(root: &Path, task_id: &str) -> Result<Vec<Map<String, Value>>, WorkflowError> {
    let mut attempts: Vec<Map<String, Value>> = Vec::new();
    for number in 1..=MAX_ATTEMPTS {
        let path = attempt_path(root, task_id, number);
        if exists(&path) {
            let entry = read(&path)?;
            require(
                text(&entry, "mode") == Some("repair-attempt-reservation"),
                "Invalid reservation",
            )?;
            require(
                entry.get("number") == Some(&json!(number))
                    && text(&entry, "task_id") == Some(task_id),
                "Reservation mismatch",
            )?;
            attempts.push(entry);
        } else if !attempts.is_empty() {
            require(
                !((number + 1)..=MAX_ATTEMPTS)
                    .any(|later| attempt_path(root, task_id, later).exists()),
                "Noncontiguous attempt history",
            )?;
        }
    }
    Ok(attempts)
}

fn is_terminal(record: &Map<String, Value>) -> bool {
    matches!(text(record, "outcome"), Some("passed" | "abandoned"))
}

/// Read immutable reservations, without replay or dispatch.
///
/// # Errors
/// An unknown task, or reservation or disposition history that is invalid, tampered with or
/// not contiguous.
pub fn inspect_repair(state: &Path, task_id: &str) -> Result<Value, WorkflowError> {
    let (root, opened) = sequence(state, task_id)?;
    let attempts = read_attempts(&root, task_id)?;
    let dispositions = read_dispositions(&root, task_id, &attempts)?;
    let terminal = dispositions.last().is_some_and(is_terminal);
    let pending = !attempts.is_empty() && dispositions.len() < attempts.len();
    let next_stage = if terminal {
        "finished"
    } else if pending {
        "await-attempt-disposition"
    } else if attempts.len() == MAX_ATTEMPTS {
        "exhausted"
    } else {
        "may-reserve-attempt"
    };
    Ok(json!({
        "dispositions": dispositions,
        "terminal": terminal,
        "mode": "repair-inspection",
        "task_id": task_id,
        "opened": opened,
        "attempts": attempts,
        "attempts_used": attempts.len(),
        "remaining": MAX_ATTEMPTS - attempts.len(),
        "next_stage": next_stage,
        "automated_dispatch": false,
    }))
}

/// Number of reservations in an inspection report.
pub(crate) fn attempts_used(report: &Value) -> Option<usize> {
    report
        .get("attempts_used")
        .and_then(Value::as_u64)
        .and_then(|used| usize::try_from(used).ok())
}

/// Reserve a one-shot attempt; cannot imply inference or execution approval.
///
/// # Errors
/// Missing authorization, an exhausted or closed sequence, a prior attempt without a
/// disposition, or an unreadable history.
pub fn reserve_repair(
    state: &Path,
    task_id: &str,
    authorized: bool,
) -> Result<Value, WorkflowError> {
    require(
        authorized,
        "Explicit authorization to reserve a repair attempt required",
    )?;
    let (root, opened) = sequence(state, task_id)?;
    let report = inspect_repair(state, task_id)?;
    let used = match attempts_used(&report) {
        Some(used) if used < MAX_ATTEMPTS => used,
        _ => {
            return Err(WorkflowError::msg(
                "Invalid or exhausted repair attempt count",
            ));
        }
    };
    require(
        text_of(&report, "next_stage") == Some("may-reserve-attempt"),
        "Prior attempt pending or sequence closed",
    )?;
    let number = used + 1;
    let evidence = json!({
        "mode": "repair-attempt-reservation",
        "task_id": task_id,
        "number": number,
        "source_failure_sha256": required(&opened, "failure_sha256")?,
        "checkpoint_sha256": required(&opened, "checkpoint_sha256")?,
        "stage": "await-separate-worker-and-reviewer-authorization",
        "worker_dispatched": false,
        "reviewer_dispatched": false,
        "files_applied": false,
        "generated_code_executed": false,
        "human_decision_pending": true,
    });
    let path = attempt_path(&root, task_id, number);
    let digest = write_once(&path, &evidence)?;
    Ok(with_evidence(evidence, &path, &digest))
}

/// Read a contiguous, hash-linked history of operator dispositions.
fn read_dispositions(
    root: &Path,
    task_id: &str,
    attempts: &[Map<String, Value>],
) -> Result<Vec<Map<String, Value>>, WorkflowError> {
    let mut completed: Vec<Map<String, Value>> = Vec::new();
    for number in 1..=attempts.len() {
        let path = disposition_path(root, task_id, number);
        if !exists(&path) {
            break;
        }
        let record = read(&path)?;
        let (_, digest) = load(&attempt_path(root, task_id, number))?;
        let linked = text(&record, "mode") == Some("repair-attempt-disposition")
            && text(&record, "task_id") == Some(task_id)
            && record.get("number") == Some(&json!(number))
            && text(&record, "reservation_sha256") == Some(digest.as_str())
            && text(&record, "outcome").is_some_and(|outcome| OUTCOMES.contains(&outcome));
        require(linked, "Invalid or unlinked repair disposition")?;
        require(
            flag(&record, "automatic_execution_authorized", false),
            "Repair disposition must not authorize execution",
        )?;
        completed.push(record);
    }
    require(
        !((completed.len() + 1)..=attempts.len())
            .any(|later| disposition_path(root, task_id, later).exists()),
        "Noncontiguous repair disposition history",
    )?;
    let early_terminal = completed
        .split_last()
        .is_some_and(|(_, earlier)| earlier.iter().any(is_terminal));
    require(
        !early_terminal,
        "Terminal repair attempt followed by another disposition",
    )?;
    Ok(completed)
}

/// Operator-reviewed attempt outcome and the evidence that backs it.
#[derive(Debug, Clone, Copy)]
pub struct DispositionRequest<'a> {
    /// State directory.
    pub state: &'a Path,
    /// Task identifier.
    pub task_id: &'a str,
    /// `rejected`, `failed`, `passed` or `abandoned`.
    pub outcome: &'a str,
    /// Native dispatch review receipt.
    pub review: Option<&'a Path>,
    /// Sandbox functional verification receipt.
    pub verification: Option<&'a Path>,
    /// Explicit operator authorization.
    pub authorized: bool,
}

fn check_replacements(
    reviewed: &Map<String, Value>,
    claim: &Map<String, Value>,
) -> Result<(), WorkflowError> {
    let (
        Some(Value::Array(replacements)),
        Some(Value::Object(preimages)),
        Some(Value::Array(owned)),
    ) = (
        reviewed.get("replacements"),
        claim.get("preimages"),
        claim.get("owned_paths"),
    )
    else {
        return Err(WorkflowError::msg(
            "Malformed reviewed replacement evidence",
        ));
    };
    require(
        (1..=MAX_REPAIR_FILES).contains(&replacements.len()),
        "Malformed reviewed replacement evidence",
    )?;
    let mut seen: Vec<&str> = Vec::new();
    for replacement in replacements {
        let Value::Object(replacement) = replacement else {
            return Err(WorkflowError::msg("Malformed reviewed replacement"));
        };
        let valid = match (
            text(replacement, "path"),
            text(replacement, "content"),
            text(replacement, "expected_sha256"),
            text(replacement, "content_sha256"),
        ) {
            (Some(name), Some(content), Some(expected), Some(digest)) => {
                let listed = owned.iter().any(|item| item.as_str() == Some(name));
                let bound = preimages.get(name).and_then(Value::as_str) == Some(expected);
                let unseen = !seen.contains(&name);
                seen.push(name);
                listed
                    && unseen
                    && content.len() <= MAX_REPLACEMENT_BYTES
                    && bound
                    && sha256_hex(content.as_bytes()) == digest
            }
            _ => false,
        };
        require(valid, "Invalid reviewed replacement provenance")?;
    }
    Ok(())
}

/// Validate a native dispatch review against its claim; returns the review digest.
fn check_review(
    ctx: (&Path, &str, usize),
    opened: &Map<String, Value>,
    reservation_hash: &str,
    review: &Path,
    outcome: &str,
) -> Result<String, WorkflowError> {
    let (root, task_id, used) = ctx;
    let (reviewed, review_hash) = load(review)?;
    let native_review = root.join(format!("{task_id}-repair-{used}-review.json"));
    let claim_path = root.join(format!("{task_id}-repair-{used}-dispatch.json"));
    require(
        resolved(review) == resolved(&native_review)
            && claim_path.is_file()
            && !claim_path.is_symlink_lexical(),
        "Review must originate from native repair dispatch",
    )?;
    let (claim, claim_hash) = load(&claim_path)?;
    let same = |name: &str| reviewed.get(name) == claim.get(name);
    let provenance = text(&claim, "mode") == Some("repair-dispatch-claim")
        && text(&claim, "task_id") == Some(task_id)
        && claim.get("number") == Some(&json!(used))
        && text(&claim, "reservation_sha256") == Some(reservation_hash)
        && text(&reviewed, "claim_sha256") == Some(claim_hash.as_str())
        && reviewed.get("checkpoint_sha256") == opened.get("checkpoint_sha256")
        && reviewed.get("source_failure_sha256") == opened.get("failure_sha256")
        && reviewed.get("baseline") == opened.get("baseline")
        && reviewed.get("workspace") == opened.get("workspace")
        && same("owned_paths")
        && same("worker_model")
        && same("reviewer_model");
    require(provenance, "Review dispatch provenance mismatch")?;
    check_replacements(&reviewed, &claim)?;
    let expected_verdict = if outcome == "rejected" {
        "changes_required"
    } else {
        "accept"
    };
    let bound = text(&reviewed, "mode") == Some("repair-independent-review")
        && text(&reviewed, "task_id") == Some(task_id)
        && reviewed.get("number") == Some(&json!(used))
        && text(&reviewed, "reservation_sha256") == Some(reservation_hash)
        && reviewed.get("worker_model") != reviewed.get("reviewer_model")
        && reviewed.get("worker_model").is_some_and(Value::is_string)
        && reviewed.get("reviewer_model").is_some_and(Value::is_string)
        && text(&reviewed, "verdict") == Some(expected_verdict);
    require(bound, "Review evidence is not bound to this attempt")?;
    Ok(review_hash)
}

fn check_verification(
    path: &Path,
    task_id: &str,
    opened: &Map<String, Value>,
    outcome: &str,
) -> Result<String, WorkflowError> {
    let (verified, digest) = load(path)?;
    let bound = text(&verified, "mode") == Some("sandboxed-functional-verification")
        && text(&verified, "task_id") == Some(task_id)
        && flag(&verified, "tests_executed", true)
        && flag(&verified, "sandbox_started", true)
        && flag(&verified, "timed_out", false)
        && flag(&verified, "functional_verified", outcome == "passed")
        && verified.get("checkpoint_sha256") == opened.get("checkpoint_sha256")
        && verified.get("baseline") == opened.get("baseline")
        && verified.get("workspace") == opened.get("workspace");
    require(
        bound,
        "Verification evidence is not bound to repair sequence",
    )?;
    Ok(digest)
}

/// Record an operator-reviewed attempt outcome; never apply or execute changes.
///
/// Rejected and abandoned attempts require no generated-code evidence. Failed and passed
/// outcomes require a separately generated sandbox receipt.
///
/// # Errors
/// Missing authorization, an unknown outcome, no pending attempt, or review or verification
/// evidence that is missing, unexpected, or not bound to this attempt.
pub fn record_repair_disposition(request: &DispositionRequest<'_>) -> Result<Value, WorkflowError> {
    let DispositionRequest {
        state,
        task_id,
        outcome,
        review,
        verification,
        authorized,
    } = *request;
    require(
        authorized,
        "Explicit authorization to record disposition required",
    )?;
    require(OUTCOMES.contains(&outcome), "Unknown repair outcome")?;
    let (root, opened) = sequence(state, task_id)?;
    let report = inspect_repair(state, task_id)?;
    require(
        text_of(&report, "next_stage") == Some("await-attempt-disposition"),
        "No pending attempt",
    )?;
    let used = match attempts_used(&report) {
        Some(used) if used >= 1 => used,
        _ => return Err(WorkflowError::msg("Malformed attempt count")),
    };
    let (_, reservation_hash) = load(&attempt_path(&root, task_id, used))?;
    let mut review_hash: Option<String> = None;
    let mut verification_hash: Option<String> = None;
    if matches!(outcome, "rejected" | "failed" | "passed") {
        let Some(review) = review else {
            return Err(WorkflowError::msg("Independent review evidence required"));
        };
        review_hash = Some(check_review(
            (&root, task_id, used),
            &opened,
            &reservation_hash,
            review,
            outcome,
        )?);
    }
    if matches!(outcome, "failed" | "passed") {
        let Some(verification) = verification else {
            return Err(WorkflowError::msg(
                "Functional verification evidence required",
            ));
        };
        verification_hash = Some(check_verification(verification, task_id, &opened, outcome)?);
    }
    require(
        outcome != "abandoned" || review.is_none(),
        "Abandoned attempt must not attach a review",
    )?;
    require(
        !matches!(outcome, "rejected" | "abandoned") || verification.is_none(),
        "Unexpected verification evidence",
    )?;
    let next_stage = if matches!(outcome, "passed" | "abandoned") {
        "finished"
    } else if used == MAX_ATTEMPTS {
        "exhausted"
    } else {
        "may-reserve-attempt"
    };
    let evidence = json!({
        "mode": "repair-attempt-disposition",
        "task_id": task_id,
        "number": used,
        "reservation_sha256": reservation_hash,
        "review_sha256": review_hash,
        "verification_sha256": verification_hash,
        "outcome": outcome,
        "automatic_execution_authorized": false,
        "human_decision_pending": true,
        "next_stage": next_stage,
    });
    let destination = disposition_path(&root, task_id, used);
    let digest = write_once(&destination, &evidence)?;
    Ok(with_evidence(evidence, &destination, &digest))
}
