//! Append-only checkpoint connecting reviewed proposals to verified worktrees.
//!
//! This is a bookkeeping gate, not permission to run generated code or repair it.

use crate::multiagent::runtime::fail;
use crate::util::PathLexical as _;
use crate::util::is_slug_id;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::multiagent::runtime::WorkflowError;
use crate::util::{pretty_json_value, py_path_str, sha256_hex};

const MAX_BYTES: u64 = 128 * 1024;
const MAX_CHECKPOINT_FILES: usize = 16;
const MAX_REPAIR_ATTEMPTS: u8 = 3;

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

pub(crate) fn load(path: &Path) -> Result<(Map<String, Value>, String), WorkflowError> {
    let bounded = !path.is_symlink_lexical()
        && path.is_file()
        && path.metadata().is_ok_and(|meta| meta.len() <= MAX_BYTES);
    if !bounded {
        return fail("Evidence must be a regular, bounded, non-symlink file");
    }
    let payload = fs::read(path)?;
    let Ok(record) = serde_json::from_slice::<Value>(&payload) else {
        return fail("Evidence is not valid JSON");
    };
    let Value::Object(record) = record else {
        return fail("Evidence must be a JSON object");
    };
    Ok((record, sha256_hex(&payload)))
}

/// `'..' in Path(name).parts`, `name.startswith('/')` and friends.
fn is_owned_path(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.split('/').any(|part| part == "..")
        && !name.contains('\\')
}

pub(crate) fn files(
    record: &Map<String, Value>,
) -> Result<BTreeMap<String, String>, WorkflowError> {
    let rows = match record.get("files") {
        Some(Value::Array(rows)) if !rows.is_empty() && rows.len() <= MAX_CHECKPOINT_FILES => rows,
        _ => return fail("Evidence file list is invalid"),
    };
    let mut result = BTreeMap::new();
    for row in rows {
        let Value::Object(row) = row else {
            return fail("Invalid evidence file entry");
        };
        let (Some(name), Some(digest)) = (
            row.get("path").and_then(Value::as_str),
            row.get("sha256").and_then(Value::as_str),
        ) else {
            return fail("Invalid evidence path or digest");
        };
        if !is_owned_path(name) {
            return fail("Invalid owned path");
        }
        if !is_lower_hex(digest, 64) || result.contains_key(name) {
            return fail("Invalid or repeated file digest");
        }
        result.insert(name.to_owned(), digest.to_owned());
    }
    Ok(result)
}

fn flag(record: &Map<String, Value>, key: &str, expected: bool) -> bool {
    record.get(key) == Some(&Value::Bool(expected))
}

fn text(record: &Map<String, Value>, key: &str, expected: &str) -> bool {
    record.get(key).and_then(Value::as_str) == Some(expected)
}

fn check_receipts(
    review: &Map<String, Value>,
    verified: &Map<String, Value>,
) -> Result<(), WorkflowError> {
    let accepted = text(review, "mode", "live-reviewed-proposal-only")
        && text(review, "verdict", "accept")
        && flag(review, "applied", false)
        && flag(review, "human_decision_pending", true);
    if !accepted {
        return fail("Review receipt is not an accepted, pending proposal");
    }
    let integrity_only = text(verified, "mode", "read-only-integrity-verification")
        && flag(verified, "integrity_verified", true)
        && flag(verified, "tests_executed", false)
        && flag(verified, "generated_code_executed", false)
        && flag(verified, "human_decision_pending", true);
    if !integrity_only {
        return fail("Verification receipt is not a valid integrity-only result");
    }
    for (review_key, verified_key) in [
        ("task_id", "task_id"),
        ("repository", "repository"),
        ("head", "baseline"),
    ] {
        if review.get(review_key) != verified.get(verified_key) {
            return Err(WorkflowError::Message(format!(
                "Review and verification mismatch: {review_key}"
            )));
        }
    }
    Ok(())
}

/// Python `sorted(value)` over a JSON value, or `None` where Python would raise or differ.
fn sorted_paths(value: Option<&Value>) -> Option<Vec<String>> {
    let mut names: Vec<String> = match value {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<_>>()?,
        Some(Value::String(text)) => text.chars().map(String::from).collect(),
        Some(Value::Object(map)) => map.keys().cloned().collect(),
        Some(_) => return None,
    };
    names.sort();
    Some(names)
}

/// Link accepted review and passed integrity results; do not execute code.
///
/// Writes `evidence` exclusively (mode `0o600` on Unix) and returns the checkpoint record plus
/// its `evidence` path and `evidence_sha256`, as a JSON object with sorted keys.
///
/// # Errors
/// Missing authorization, an unreadable or malformed receipt, receipts that do not link up,
/// an oversized checkpoint, or an evidence destination that is not a new file within a real
/// directory.
pub fn record_checkpoint(
    receipt: &Path,
    verification: &Path,
    evidence: &Path,
    authorized: bool,
) -> Result<Value, WorkflowError> {
    if !authorized {
        return fail("Explicit authorization to record a checkpoint is required");
    }
    let (review, review_hash) = load(receipt)?;
    let (verified, verification_hash) = load(verification)?;
    let baseline = match review.get("head").and_then(Value::as_str) {
        Some(head) if is_lower_hex(head, 40) => head,
        _ => return fail("Review baseline invalid"),
    };
    check_receipts(&review, &verified)?;
    let original_files = files(&review)?;
    let verified_files = files(&verified)?;
    let verified_names: Vec<&String> = verified_files.keys().collect();
    let changed = sorted_paths(verified.get("changed_paths"));
    let same_paths = changed.is_some_and(|names| names.iter().eq(verified_names.iter().copied()));
    if original_files != verified_files || !same_paths {
        return fail("Reviewed and verified paths or digests differ");
    }
    let task_id = match review.get("task_id").and_then(Value::as_str) {
        Some(id) if is_slug_id(id) => id,
        _ => return fail("Invalid task identifier"),
    };
    let workspace = match verified.get("workspace").and_then(Value::as_str) {
        Some(path) if path.starts_with('/') => path,
        _ => return fail("Verification worktree identity invalid"),
    };
    let branch = format!("harness/{task_id}");
    if !text(&verified, "branch", &branch) {
        return fail("Verification worktree identity invalid");
    }
    let Some(repository) = review.get("repository") else {
        return fail("'repository'");
    };
    let listed: Vec<Value> = original_files
        .iter()
        .map(|(path, digest)| json!({"path": path, "sha256": digest}))
        .collect();
    let mut result = json!({
        "mode": "integrity-checkpoint",
        "task_id": task_id,
        "repository": repository,
        "baseline": baseline,
        "workspace": workspace,
        "branch": branch,
        "review_receipt_sha256": review_hash,
        "verification_receipt_sha256": verification_hash,
        "files": listed,
        "integrity_verified": true,
        "functional_verified": false,
        "tests_executed": false,
        "generated_code_executed": false,
        "repair_attempts": 0,
        "max_repair_attempts": MAX_REPAIR_ATTEMPTS,
        "next_stage": "requires-separate-functional-execution-authorization",
        "committed": false,
        "merged": false,
        "human_decision_pending": true,
    });
    let payload = format!("{}\n", pretty_json_value(&result)?).into_bytes();
    if payload.len() as u64 > MAX_BYTES {
        return fail("Checkpoint evidence too large");
    }
    write_new(evidence, &payload)?;
    if let Value::Object(map) = &mut result {
        map.insert("evidence".to_owned(), json!(py_path_str(evidence)));
        map.insert("evidence_sha256".to_owned(), json!(sha256_hex(&payload)));
    }
    Ok(result)
}

/// Create `evidence` exclusively inside an existing real directory and write `payload`.
fn write_new(evidence: &Path, payload: &[u8]) -> Result<(), WorkflowError> {
    let parent = evidence
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if evidence.is_symlink_lexical()
        || evidence.exists()
        || parent.is_symlink_lexical()
        || !parent.is_dir()
    {
        return fail("Checkpoint evidence destination must be unused within a real directory");
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(evidence)?.write_all(payload)?;
    Ok(())
}
