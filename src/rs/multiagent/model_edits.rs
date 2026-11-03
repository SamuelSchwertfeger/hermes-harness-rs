//! Validate model-produced file proposals without applying them.
//!
//! A proposal is data, not a command. It cannot rename, delete, execute, commit,
//! or mutate a worktree. Applying changes requires a separate qualified boundary.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::multiagent::runtime::WorkflowError;
use crate::multiagent::runtime::fail;
use crate::util::sha256_hex;

/// Maximum size of a whole proposal payload in bytes.
pub const MAX_PROPOSAL_BYTES: usize = 128 * 1024;
/// Maximum size of one proposed file in bytes.
pub const MAX_FILE_BYTES: usize = 48 * 1024;
/// Maximum number of proposed files.
pub const MAX_FILES: usize = 16;

/// Model-proposed UTF-8 file replacement under assigned ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedFile {
    /// Relative path inside the worktree.
    pub path: String,
    /// Proposed file content.
    pub content: String,
    /// SHA-256 hex digest of the UTF-8 content.
    pub sha256: String,
}

/// Admitted proposal for one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditProposal {
    /// Task the proposal belongs to.
    pub task_id: String,
    /// Proposed files in payload order.
    pub files: Vec<ProposedFile>,
}

/// True when `name` equals an owned scope or sits below one at a `/` boundary.
pub(crate) fn is_owned(name: &str, scopes: &[String]) -> bool {
    scopes.iter().any(|scope| {
        name == scope
            || name
                .strip_prefix(scope.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Expose the proposal path predicate for external parser assertions.
#[doc(hidden)]
#[must_use]
pub fn is_lexically_safe_path_for_testing(name: &str) -> bool {
    is_lexically_safe_path(name)
}

/// Relative, normalized, non-`.git` path without backslashes or NUL bytes.
pub(crate) fn is_lexically_safe_path(name: &str) -> bool {
    !(name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || name
            .split('/')
            .any(|piece| matches!(piece, "" | "." | ".."))
        || name.split('/').next() == Some(".git"))
}

fn valid_owned_path(value: &str, scopes: &[String]) -> bool {
    is_lexically_safe_path(value) && is_owned(value, scopes)
}

fn safe_path<'a>(value: Option<&'a Value>, scopes: &[String]) -> Result<&'a str, WorkflowError> {
    let Some(name) = value.and_then(Value::as_str) else {
        return fail("Proposal path must be a string");
    };
    if !valid_owned_path(name, scopes) {
        return fail("Proposal touches a forbidden or unowned path");
    }
    Ok(name)
}

fn has_exact_keys(row: &serde_json::Map<String, Value>, keys: &[&str]) -> bool {
    row.len() == keys.len() && keys.iter().all(|key| row.contains_key(*key))
}

fn parse_file_row(
    row: &Value,
    owned_paths: &[String],
    seen: &mut BTreeSet<String>,
) -> Result<ProposedFile, WorkflowError> {
    let Some(map) = row
        .as_object()
        .filter(|map| has_exact_keys(map, &["path", "content"]))
    else {
        return fail("Invalid proposed file schema");
    };
    let name = safe_path(map.get("path"), owned_paths)?;
    let content = match map.get("content").and_then(Value::as_str) {
        Some(text) if text.len() <= MAX_FILE_BYTES => text,
        _ => return fail("Invalid proposed file content"),
    };
    if !seen.insert(name.to_owned()) {
        return fail("Duplicate proposed path");
    }
    Ok(ProposedFile {
        path: name.to_owned(),
        content: content.to_owned(),
        sha256: sha256_hex(content.as_bytes()),
    })
}

/// Admit a bounded file proposal; no filesystem access or network calls.
///
/// # Errors
///
/// Returns a [`WorkflowError`] for oversized or malformed payloads, schema or task identity
/// mismatches, a bad file count, unsafe or unowned paths, invalid content, or duplicates.
pub fn parse_edit_proposal(
    payload: &str,
    task_id: &str,
    owned_paths: &[String],
) -> Result<EditProposal, WorkflowError> {
    if payload.len() > MAX_PROPOSAL_BYTES {
        return fail("Oversized model proposal");
    }
    let document =
        serde_json::from_str::<Value>(payload).map_err(|source| WorkflowError::AdmissionJson {
            message: "Malformed model proposal".to_owned(),
            source,
        })?;
    let Some(map) = document
        .as_object()
        .filter(|map| has_exact_keys(map, &["task_id", "files"]))
    else {
        return fail("Invalid proposal schema");
    };
    if map.get("task_id").and_then(Value::as_str) != Some(task_id) {
        return fail("Proposal task identity mismatch");
    }
    let rows = match map.get("files").and_then(Value::as_array) {
        Some(rows) if (1..=MAX_FILES).contains(&rows.len()) => rows,
        _ => return fail("Invalid proposal file count"),
    };
    let mut seen = BTreeSet::new();
    let files = rows
        .iter()
        .map(|row| parse_file_row(row, owned_paths, &mut seen))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(EditProposal {
        task_id: task_id.to_owned(),
        files,
    })
}
