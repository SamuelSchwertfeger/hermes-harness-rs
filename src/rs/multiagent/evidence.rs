//! Read-only, bounded evidence bundle for independent model review.
//!
//! Reviewer input comes from persisted task receipts, not arbitrary shell access.
//! This module does not claim OS confinement or authorize a merge.

use crate::util::PathLexical as _;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::runtime::{RunState, TaskState, WorkflowError, inspect};
use crate::util::sha256_hex;

/// Maximum accepted receipt size in bytes.
pub const MAX_RECEIPT_BYTES: u64 = 16 * 1024;
/// Maximum number of reviewed tasks.
pub const MAX_REVIEW_NODES: usize = 32;

/// Digest evidence of one completed task receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReceiptEvidence {
    /// Node identifier.
    pub node: String,
    /// Task identifier recorded in the receipt (`null` when absent).
    pub task_id: Value,
    /// Worker output digest recorded in the receipt.
    pub worker_digest: Value,
    /// Reviewer output digest recorded in the receipt.
    pub review_digest: Value,
    /// SHA-256 of the receipt bytes.
    pub receipt_sha256: String,
}

/// Immutable review evidence; never authorizes a merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewBundle {
    /// Run identifier.
    pub run_id: String,
    /// Run state.
    pub state: RunState,
    /// One entry per task, in plan order.
    pub receipt_evidence: Vec<ReceiptEvidence>,
    /// Always `false`.
    pub authorizes_merge: bool,
}

fn receipt(path: &Path) -> Result<(Value, String), WorkflowError> {
    if path.is_symlink_lexical() || !path.is_file() {
        return Err(WorkflowError::msg("Review receipt missing or symlinked"));
    }
    if std::fs::metadata(path)?.len() > MAX_RECEIPT_BYTES {
        return Err(WorkflowError::msg("Review receipt oversized"));
    }
    let data = std::fs::read(path)?;
    let value: Value = serde_json::from_slice(&data)?;
    if !value.is_object() {
        return Err(WorkflowError::msg("Review receipt is not an object"));
    }
    Ok((value, sha256_hex(&data)))
}

/// Extract immutable receipt digests and decisions without redispatch.
///
/// # Errors
/// Incomplete run, or a missing, symlinked, oversized, malformed or not approval-ready receipt.
pub fn review_bundle(state_root: &Path, run_id: &str) -> Result<ReviewBundle, WorkflowError> {
    let summary = inspect(state_root, run_id)?;
    if summary.tasks.len() > MAX_REVIEW_NODES {
        return Err(WorkflowError::msg("Invalid number of reviewed tasks"));
    }
    let directory = state_root.join(run_id);
    if directory.is_symlink_lexical() {
        return Err(WorkflowError::msg("Symlink run directory"));
    }
    let mut receipts = Vec::new();
    for task in &summary.tasks {
        if task.state != TaskState::Completed {
            return Err(WorkflowError::msg(
                "Review requires fully completed task evidence",
            ));
        }
        let (record, digest) = receipt(&directory.join(format!("{}.receipt.json", task.node)))?;
        if record.get("state").and_then(Value::as_str) != Some("awaiting_approval") {
            return Err(WorkflowError::msg("Task receipt is not approval-ready"));
        }
        let field = |name: &str| record.get(name).cloned().unwrap_or(Value::Null);
        receipts.push(ReceiptEvidence {
            node: task.node.clone(),
            task_id: field("task_id"),
            worker_digest: field("worker_sha256"),
            review_digest: field("review_sha256"),
            receipt_sha256: digest,
        });
    }
    Ok(ReviewBundle {
        run_id: run_id.to_owned(),
        state: summary.state,
        receipt_evidence: receipts,
        authorizes_merge: false,
    })
}
