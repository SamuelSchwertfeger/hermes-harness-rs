//! Explicit human-granted application of a reviewed receipt to a disposable worktree.
//!
//! Only creates new files. No generated-code execution, commits, pushes or merges.

use crate::multiagent::runtime::fail;
use crate::util::PathLexical as _;
use crate::util::is_slug_id;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::git::worktrees::{WorktreeError, WorktreeSpec, create_worktree};
use crate::multiagent::apply_edits::{ApplyEditsError, apply_new_files};
use crate::multiagent::model_edits::{EditProposal, parse_edit_proposal};
use crate::multiagent::runtime::WorkflowError;
use crate::native::workflows::{git, repository};
use crate::util::{ordered_json_spaced, sha256_hex, splitlines_keepends};

const MIN_DIFF_LINES: usize = 4;
const MAX_RECEIPT: u64 = 128 * 1024;
const MAX_FILES: usize = 16;

/// Failure while admitting a receipt or applying it to a worktree.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeApplyError {
    /// Receipt or workflow rule violation.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Git or ownership failure while creating the worktree.
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    /// Failure while applying the new files.
    #[error(transparent)]
    ApplyEdits(#[from] ApplyEditsError),
}

fn denied<T>(message: &str) -> Result<T, WorktreeApplyError> {
    Err(WorkflowError::msg(message).into())
}

fn is_head(value: &str) -> bool {
    value.len() == 40
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

fn read_receipt(path: &Path) -> Result<Map<String, Value>, WorkflowError> {
    let bounded = !path.is_symlink_lexical()
        && path.is_file()
        && path.metadata().is_ok_and(|meta| meta.len() <= MAX_RECEIPT);
    if !bounded {
        return fail("Expected a bounded regular receipt file");
    }
    match serde_json::from_str::<Value>(&fs::read_to_string(path)?)? {
        Value::Object(record) => Ok(record),
        _ => fail("Invalid receipt object"),
    }
}

/// Unified diff ranges default to one line when the count is omitted.
fn diff_range(text: &str) -> Option<(usize, usize)> {
    let (start, count) = text.split_once(',').unwrap_or((text, "1"));
    if [start, count]
        .iter()
        .any(|part| part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    Some((start.parse().ok()?, count.parse().ok()?))
}

fn new_file_count(line: &str) -> Option<usize> {
    let ranges = line.strip_prefix("@@ -")?.strip_suffix(" @@\n")?;
    let (old, new) = ranges.split_once(" +")?;
    let (old_start, old_count) = diff_range(old)?;
    let (new_start, new_count) = diff_range(new)?;
    (old_start == 0 && old_count == 0 && new_start == 1).then_some(new_count)
}

/// Decode only the new-file unified-diff form emitted by `live_review`.
fn content_from_new_file_diff(diff: &str, path: &str) -> Result<String, WorkflowError> {
    let lines = splitlines_keepends(diff);
    let header = format!("+++ b/{path}\n");
    let headers_ok = lines.len() >= MIN_DIFF_LINES
        && lines.first() == Some(&"--- /dev/null\n")
        && lines.get(1).copied() == Some(header.as_str());
    if !headers_ok {
        return fail("Invalid new-file diff headers");
    }
    let count = lines
        .get(2)
        .and_then(|line| new_file_count(line))
        .ok_or_else(|| WorkflowError::msg("Invalid new-file diff hunk"))?;
    let body = lines.get(3..).unwrap_or_default();
    if body.len() != count || body.iter().any(|line| !line.starts_with('+')) {
        return fail("Invalid new-file diff lines");
    }
    Ok(body.iter().filter_map(|line| line.get(1..)).collect())
}

#[derive(Serialize)]
struct DecodedFile<'a> {
    path: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ProposalPayload<'a> {
    task_id: &'a str,
    files: Vec<DecodedFile<'a>>,
}

/// Decode one receipt row into `(path, content)`.
fn decode_row(row: &Value) -> Result<(String, String), WorkflowError> {
    let schema_ok = row.as_object().is_some_and(|map| {
        map.len() == 3
            && ["path", "sha256", "diff"]
                .iter()
                .all(|key| map.contains_key(*key))
    });
    if !schema_ok {
        return fail("Invalid file receipt schema");
    }
    let field = |key: &str| row.get(key).and_then(Value::as_str);
    let (Some(path), Some(digest), Some(diff)) = (field("path"), field("sha256"), field("diff"))
    else {
        return fail("Invalid file receipt fields");
    };
    let content = content_from_new_file_diff(diff, path)?;
    if sha256_hex(content.as_bytes()) != digest {
        return fail("Receipt content hash mismatch");
    }
    Ok((path.to_owned(), content))
}

/// Admit a receipt record: check every file's diff against its digest, then parse the proposal.
///
/// Returns the task identifier, the owned paths and the admitted proposal.
///
/// # Errors
/// Invalid task identifier, file count or file schema, a malformed diff, a content hash
/// mismatch, or a proposal rejected by [`parse_edit_proposal`].
pub(crate) fn admitted_proposal(
    record: &Map<String, Value>,
) -> Result<(String, Vec<String>, EditProposal), WorkflowError> {
    let task_id = match record.get("task_id").and_then(Value::as_str) {
        Some(id) if is_slug_id(id) => id,
        _ => return fail("Invalid task identifier"),
    };
    let rows = match record.get("files") {
        Some(Value::Array(rows)) if (1..=MAX_FILES).contains(&rows.len()) => rows,
        _ => return fail("Invalid file count in receipt"),
    };
    let decoded = rows.iter().map(decode_row).collect::<Result<Vec<_>, _>>()?;
    let owned: Vec<String> = decoded.iter().map(|(path, _)| path.clone()).collect();
    let payload = ordered_json_spaced(&ProposalPayload {
        task_id,
        files: decoded
            .iter()
            .map(|(path, content)| DecodedFile { path, content })
            .collect(),
    })?;
    let proposal = parse_edit_proposal(&payload, task_id, &owned)?;
    Ok((task_id.to_owned(), owned, proposal))
}

fn expected_receipt(record: &Map<String, Value>, root: &Path) -> bool {
    record.get("repository") == Some(&json!(root.to_string_lossy()))
        && record.get("mode") == Some(&json!("live-reviewed-proposal-only"))
        && record.get("verdict") == Some(&json!("accept"))
        && record.get("applied") == Some(&json!(false))
        && record.get("human_decision_pending") == Some(&json!(true))
}

/// Create and audit one disposable worktree after an explicit application grant.
///
/// # Errors
/// Missing authorization, an ineligible or malformed receipt, a repository whose `HEAD` moved
/// since review, or any failure creating the worktree and applying the files.
pub fn apply_reviewed_receipt(
    repo: &Path,
    receipt: &Path,
    workspace: &Path,
    authorized: bool,
) -> Result<Value, WorktreeApplyError> {
    apply_reviewed_receipt_with_root(repo, receipt, workspace, authorized, repository)
}

/// Apply a reviewed receipt with a caller-supplied repository resolver.
///
/// # Errors
/// Same as [`apply_reviewed_receipt`], plus any error returned by `resolve_root`.
pub fn apply_reviewed_receipt_with_root(
    repo: &Path,
    receipt: &Path,
    workspace: &Path,
    authorized: bool,
    resolve_root: impl FnOnce(&Path) -> Result<PathBuf, WorkflowError>,
) -> Result<Value, WorktreeApplyError> {
    if !authorized {
        return denied("Explicit human authorization for worktree application is required");
    }
    let root = resolve_root(repo)?;
    let record = read_receipt(receipt)?;
    if !expected_receipt(&record, &root) {
        return denied("Receipt not eligible for worktree application");
    }
    let baseline = match record.get("head").and_then(Value::as_str) {
        Some(head) if is_head(head) => head,
        _ => return denied("Invalid receipt baseline"),
    };
    if crate::util::py_strip(&git(&root, &["rev-parse", "HEAD"])?) != baseline {
        return denied("Repository HEAD changed since review");
    }
    let (task_id, owned, proposal) = admitted_proposal(&record)?;
    let spec = WorktreeSpec {
        branch: format!("harness/{task_id}"),
        ticket_id: task_id.clone(),
        owned_paths: owned.clone(),
    };
    // The existing helper refuses dirty sources, reused branches and existing workspaces.
    let created = create_worktree(&root, workspace, &spec)?;
    if crate::util::py_strip(&git(&created, &["rev-parse", "HEAD"])?) != baseline {
        return denied(
            "Source HEAD changed since reviewed proposal; retain worktree for inspection",
        );
    }
    let result = apply_new_files(&created, &proposal, &owned, baseline, true)?;
    Ok(json!({
        "mode": "authorized-disposable-worktree",
        "task_id": task_id,
        "workspace": created.to_string_lossy(),
        "baseline": baseline,
        "changed_paths": result.audit.changed_paths,
        "reviewer_verdict": "accept",
        "generated_code_executed": false,
        "committed": false,
        "merged": false,
        "human_decision_pending": true,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(
        missing_docs,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::pedantic,
        clippy::nursery
    )]
    //! Receipt admission tests: no model calls or worktree writes.

    use super::{admitted_proposal, apply_reviewed_receipt, apply_reviewed_receipt_with_root};
    use crate::util::sha256_hex;
    use serde_json::{Map, Value, json};

    const CONTENT: &str = "def add(a: int, b: int) -> int:\n    return a + b\n";
    const DIFF: &str = "--- /dev/null\n+++ b/smoke_math.py\n@@ -0,0 +1,2 @@\n\
+def add(a: int, b: int) -> int:\n+    return a + b\n";

    fn record() -> Map<String, Value> {
        let Value::Object(map) = json!({
            "task_id": "new-smoke",
            "files": [{
                "path": "smoke_math.py",
                "sha256": sha256_hex(CONTENT.as_bytes()),
                "diff": DIFF,
            }],
        }) else {
            panic!("object literal");
        };
        map
    }

    #[test]
    fn receipt_admits_matching_hash_and_content() {
        let (task, owned, proposal) = admitted_proposal(&record()).unwrap();
        assert_eq!(task, "new-smoke");
        assert_eq!(owned, ["smoke_math.py"]);
        assert_eq!(proposal.files[0].content, CONTENT);
    }

    #[test]
    fn one_line_new_file_round_trips() {
        let content = "VALUE = 7\n";
        let diff = crate::native::live_review::new_file_diff(content, "smoke_math.py");
        assert!(diff.contains("@@ -0,0 +1 @@\n"));
        let mut receipt = record();
        receipt["files"][0]["diff"] = json!(diff);
        receipt["files"][0]["sha256"] = json!(sha256_hex(content.as_bytes()));
        let (_, _, proposal) = admitted_proposal(&receipt).unwrap();
        assert_eq!(proposal.files[0].content, content);
        assert_eq!(super::diff_range("0"), Some((0, 1)));
        assert_eq!(super::diff_range("1"), Some((1, 1)));
        assert_eq!(super::diff_range("0,0"), Some((0, 0)));
    }

    #[test]
    fn receipt_rejects_tampered_diff() {
        let mut record = record();
        record["files"][0]["diff"] = json!(DIFF.replace("a + b", "a - b"));
        let error = admitted_proposal(&record).unwrap_err();
        assert!(error.to_string().contains("hash mismatch"), "{error}");
    }

    #[test]
    fn receipt_rejects_path_escape() {
        let mut record = record();
        record["files"][0]["path"] = json!("../escape.py");
        record["files"][0]["diff"] = json!(DIFF.replace("smoke_math.py", "../escape.py"));
        let error = admitted_proposal(&record).unwrap_err().to_string();
        assert!(
            error.contains("unowned") || error.contains("forbidden"),
            "{error}"
        );
    }

    #[test]
    fn rejected_review_never_creates_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let mut receipt = record();
        receipt.insert("repository".to_owned(), json!(tmp.path().to_string_lossy()));
        receipt.insert("mode".to_owned(), json!("live-reviewed-proposal-only"));
        receipt.insert("verdict".to_owned(), json!("changes_required"));
        receipt.insert("applied".to_owned(), json!(false));
        receipt.insert("human_decision_pending".to_owned(), json!(true));
        let path = tmp.path().join("receipt.json");
        std::fs::write(&path, Value::Object(receipt).to_string()).unwrap();
        let workspace = tmp.path().join("workspace");
        let error = apply_reviewed_receipt_with_root(tmp.path(), &path, &workspace, true, |repo| {
            Ok(repo.to_path_buf())
        })
        .unwrap_err();
        assert!(error.to_string().contains("not eligible"), "{error}");
        assert!(!workspace.exists());
    }

    #[test]
    fn application_without_grant_does_not_touch_filesystem() {
        let tmp = tempfile::tempdir().unwrap();
        let error = apply_reviewed_receipt(
            tmp.path(),
            &tmp.path().join("missing.json"),
            &tmp.path().join("workspace"),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("authorization"), "{error}");
        assert!(!tmp.path().join("workspace").exists());
    }
}
