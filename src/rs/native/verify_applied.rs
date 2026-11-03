//! Read-only integrity verification of a human-authorized reviewed worktree.

use crate::multiagent::runtime::fail;
use crate::util::PathLexical as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use crate::git::worktrees::audit_worktree;
use crate::multiagent::runtime::WorkflowError;
use crate::native::workflows::{git, repository};
use crate::native::worktree_apply::admitted_proposal;
use crate::util::{pretty_json_value, py_path_str, resolve_strict, sha256_hex};

/// Re-audit exact owned paths and bytes; persist a non-overwriting receipt.
/// # Errors
/// Rejects missing authorization, untrusted receipts, changed worktrees, or reused evidence.
pub fn verify_applied(
    repo: &Path,
    receipt: &Path,
    workspace: &Path,
    evidence: &Path,
    authorized: bool,
) -> Result<Value, WorkflowError> {
    if !authorized {
        return fail("Explicit authorization to record worktree verification is required");
    }
    let root = repository(repo)?;
    if receipt.is_symlink_lexical() || !receipt.is_file() || receipt.metadata()?.len() > 128 * 1024
    {
        return fail("Expected a bounded regular receipt file");
    }
    let record: Value = serde_json::from_str(&fs::read_to_string(receipt)?)?;
    let record = record
        .as_object()
        .ok_or_else(|| WorkflowError::msg("Invalid receipt object"))?;
    if record.get("repository") != Some(&json!(root.to_string_lossy()))
        || record.get("mode") != Some(&json!("live-reviewed-proposal-only"))
    {
        return fail("Receipt repository or mode mismatch");
    }
    if record.get("verdict") != Some(&json!("accept"))
        || record.get("applied") != Some(&json!(false))
    {
        return fail("Receipt was not an accepted proposal");
    }
    let baseline = record
        .get("head")
        .and_then(Value::as_str)
        .filter(|s| {
            s.len() == 40
                && s.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
        .ok_or_else(|| WorkflowError::msg("Invalid receipt baseline"))?;
    let (task_id, mut owned, proposal) = admitted_proposal(record)?;
    let path = resolve_strict(workspace)?;
    if path.starts_with(&root) || root.starts_with(&path) {
        return fail("Worktree must be disjoint from source");
    }
    let top = git(&path, &["rev-parse", "--show-toplevel"])?;
    if resolve_strict(Path::new(crate::util::py_strip(&top)))? != path {
        return fail("Expected a Git worktree root");
    }
    let branch =
        crate::util::py_strip(&git(&path, &["symbolic-ref", "--short", "HEAD"])?).to_owned();
    if branch != format!("harness/{task_id}") {
        return fail("Unexpected worktree branch");
    }
    if crate::util::py_strip(&git(&root, &["rev-parse", "HEAD"])?) != baseline {
        return fail("Source baseline changed");
    }
    let audit =
        audit_worktree(&path, baseline, &owned).map_err(|e| WorkflowError::msg(&e.to_string()))?;
    owned.sort();
    if audit.head != baseline || owned != audit.changed_paths {
        return fail("Worktree changed paths or HEAD differ from reviewed receipt");
    }
    let mut files = Vec::new();
    for item in &proposal.files {
        let target = path.join(&item.path);
        if target.is_symlink_lexical() || !target.is_file() {
            return fail("Proposed file missing or symlinked");
        }
        let actual = fs::read(target)?;
        if actual != item.content.as_bytes() {
            return fail(&format!("Worktree content mismatch: {}", item.path));
        }
        files.push(json!({"path":item.path,"sha256":sha256_hex(&actual)}));
    }
    if evidence.is_symlink_lexical() || evidence.exists() {
        return fail("Verification evidence path already exists");
    }
    let parent = evidence
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if parent.is_symlink_lexical() || !parent.is_dir() {
        return fail("Evidence parent must be an existing real directory");
    }
    let result = json!({"mode":"read-only-integrity-verification","task_id":task_id,"repository":root.to_string_lossy(),"workspace":path.to_string_lossy(),"baseline":baseline,"branch":branch,"changed_paths":audit.changed_paths,"files":files,"integrity_verified":true,"tests_executed":false,"generated_code_executed":false,"committed":false,"merged":false,"human_decision_pending":true});
    persist_evidence(result, evidence)
}

fn persist_evidence(mut result: Value, evidence: &Path) -> Result<Value, WorkflowError> {
    let payload = format!("{}\n", pretty_json_value(&result)?);
    if payload.len() > 128 * 1024 {
        return fail("Evidence payload too large");
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(crate::multiagent::replace_edits::nofollow_flag());
    }
    options.open(evidence)?.write_all(payload.as_bytes())?;
    if let Some(fields) = result.as_object_mut() {
        fields.insert("evidence".to_owned(), json!(py_path_str(evidence)));
        fields.insert(
            "evidence_sha256".to_owned(),
            json!(sha256_hex(payload.as_bytes())),
        );
    }
    Ok(result)
}
