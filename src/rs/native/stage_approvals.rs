//! Immutable approval checkpoints for four Hermes-native workflow stages.

use crate::util::PathLexical as _;
use crate::util::is_slug_id;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::core::delegation::{create_private_dir, write_once_value as write_once};
use crate::multiagent::runtime::WorkflowError;
use crate::native::workflows::{git, repository};
use crate::util::{resolve_lenient, sha256_hex};

/// Ordered workflow checkpoints.
pub const STAGES: [&str; 4] = [
    "lets-investigate",
    "what-we-know",
    "agents-assemble",
    "stick-the-landing",
];
/// Maximum evidence size.
pub const MAX_EVIDENCE: u64 = 262_144;
/// Maximum session length in characters.
pub const MAX_SESSION_LENGTH: usize = 128;

fn directory(state_root: &Path, run_id: &str) -> Result<PathBuf, WorkflowError> {
    let root = state_root.join(run_id);
    if !is_slug_id(run_id) || root.is_symlink_lexical() || !root.is_dir() {
        return Err(WorkflowError::msg("Invalid or missing workflow"));
    }
    Ok(root)
}

fn read(path: &Path) -> Result<Map<String, Value>, WorkflowError> {
    if path.is_symlink_lexical() || !path.is_file() || path.metadata()?.len() > MAX_EVIDENCE {
        return Err(WorkflowError::msg("Missing or untrusted workflow record"));
    }
    let value: Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    if !value.is_object() {
        return Err(WorkflowError::msg("Invalid workflow record"));
    }
    value
        .as_object()
        .cloned()
        .ok_or_else(|| WorkflowError::msg("Invalid workflow record"))
}

/// Bind a new workflow to one clean repository HEAD and Hermes session.
/// # Errors
/// Rejects invalid identities, state, dirty source, or existing workflow records.
pub fn begin(
    state_root: &Path,
    run_id: &str,
    repo: &Path,
    session: &str,
) -> Result<Value, WorkflowError> {
    if !is_slug_id(run_id)
        || session.chars().all(crate::util::is_py_space)
        || session.chars().count() > MAX_SESSION_LENGTH
    {
        return Err(WorkflowError::msg("Invalid workflow or session identity"));
    }
    if state_root.is_symlink_lexical() || !state_root.is_dir() {
        return Err(WorkflowError::msg(
            "Pre-existing real state directory required",
        ));
    }
    let root = repository(repo)?;
    if !git(&root, &["status", "--porcelain"])?.is_empty() {
        return Err(WorkflowError::msg(
            "Refusing workflow on dirty source checkout",
        ));
    }
    let dir = state_root.join(run_id);
    create_private_dir(&dir)?;
    write_once(
        &dir.join("origin.json"),
        &json!({"run_id":run_id,"repository":root.to_string_lossy(),"head":crate::util::py_strip(&git(&root,&["rev-parse","HEAD"])?),"session":session}),
    )?;
    snapshot(state_root, run_id)
}

/// Inspect the next stage and its immutable approval state.
/// # Errors
/// Rejects missing, untrusted, or mismatched workflow records.
pub fn snapshot(state_root: &Path, run_id: &str) -> Result<Value, WorkflowError> {
    let dir = directory(state_root, run_id)?;
    let mut origin = read(&dir.join("origin.json"))?;
    let mut approved = Vec::new();
    for (index, stage) in STAGES.iter().enumerate() {
        let evidence_path = dir.join(format!("{index:02}-evidence.json"));
        let grant_path = dir.join(format!("{index:02}-approval.json"));
        if !evidence_path.exists() {
            origin.insert("next_stage".to_owned(), json!(stage));
            origin.insert("approval_pending".to_owned(), json!(false));
            origin.insert("approved_stages".to_owned(), json!(approved));
            return Ok(Value::Object(origin));
        }
        let evidence = read(&evidence_path)?;
        if evidence.get("stage") != Some(&json!(stage)) {
            return Err(WorkflowError::msg("Evidence stage mismatch"));
        }
        if !grant_path.exists() {
            origin.insert("next_stage".to_owned(), json!(stage));
            origin.insert("approval_pending".to_owned(), json!(true));
            origin.insert("evidence".to_owned(), Value::Object(evidence));
            origin.insert("approved_stages".to_owned(), json!(approved));
            return Ok(Value::Object(origin));
        }
        let grant = read(&grant_path)?;
        if grant.get("sha256") != evidence.get("sha256")
            || grant.get("stage") != Some(&json!(stage))
        {
            return Err(WorkflowError::msg("Evidence approval mismatch"));
        }
        approved.push(*stage);
    }
    origin.insert("next_stage".to_owned(), Value::Null);
    origin.insert("approval_pending".to_owned(), json!(false));
    origin.insert("approved_stages".to_owned(), json!(approved));
    origin.insert("finished".to_owned(), json!(true));
    Ok(Value::Object(origin))
}

/// Submit bounded evidence for the next stage.
/// # Errors
/// Rejects ordering, session, evidence, or pinned HEAD mismatches.
pub fn submit(
    state_root: &Path,
    run_id: &str,
    stage: &str,
    evidence: &Path,
    session: &str,
) -> Result<Value, WorkflowError> {
    let current = snapshot(state_root, run_id)?;
    if current.get("next_stage") != Some(&json!(stage))
        || current.get("approval_pending") == Some(&json!(true))
    {
        return Err(WorkflowError::msg(
            "Stage out of order or awaiting approval",
        ));
    }
    if current.get("session") != Some(&json!(session)) {
        return Err(WorkflowError::msg("Session mismatch"));
    }
    if !evidence.is_file()
        || evidence.is_symlink_lexical()
        || evidence.metadata()?.len() > MAX_EVIDENCE
    {
        return Err(WorkflowError::msg("Bounded stage evidence file required"));
    }
    let source = repository(Path::new(
        current
            .get("repository")
            .and_then(Value::as_str)
            .unwrap_or("None"),
    ))?;
    if Some(crate::util::py_strip(&git(
        &source,
        &["rev-parse", "HEAD"],
    )?)) != current.get("head").and_then(Value::as_str)
    {
        return Err(WorkflowError::msg("Pinned Git HEAD changed"));
    }
    let digest = sha256_hex(&fs::read(evidence)?);
    let index = STAGES
        .iter()
        .position(|s| *s == stage)
        .ok_or_else(|| WorkflowError::msg("Stage out of order or awaiting approval"))?;
    write_once(
        &directory(state_root, run_id)?.join(format!("{index:02}-evidence.json")),
        &json!({"stage":stage,"sha256":digest,"path":resolve_lenient(evidence).to_string_lossy(),"session":session}),
    )?;
    snapshot(state_root, run_id)
}

/// Record the operator's specific affirmative grant.
/// # Errors
/// Rejects absent confirmation or mismatched stage, session, or digest.
pub fn approve(
    state_root: &Path,
    run_id: &str,
    stage: &str,
    digest: &str,
    session: &str,
    operator_confirmed: bool,
) -> Result<Value, WorkflowError> {
    if !operator_confirmed {
        return Err(WorkflowError::msg(
            "Explicit operator confirmation required",
        ));
    }
    let current = snapshot(state_root, run_id)?;
    let evidence = current.get("evidence");
    if current.get("approval_pending") != Some(&json!(true))
        || current.get("next_stage") != Some(&json!(stage))
        || current.get("session") != Some(&json!(session))
        || !evidence.is_some_and(Value::is_object)
        || evidence.and_then(|e| e.get("sha256")) != Some(&json!(digest))
    {
        return Err(WorkflowError::msg(
            "Stage, session or evidence digest mismatch",
        ));
    }
    let index = STAGES
        .iter()
        .position(|s| *s == stage)
        .ok_or_else(|| WorkflowError::msg("Stage, session or evidence digest mismatch"))?;
    write_once(
        &directory(state_root, run_id)?.join(format!("{index:02}-approval.json")),
        &json!({"stage":stage,"sha256":digest,"session":session,"operator_confirmed":true}),
    )?;
    snapshot(state_root, run_id)
}
