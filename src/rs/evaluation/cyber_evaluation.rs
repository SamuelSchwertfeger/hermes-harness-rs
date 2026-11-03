//! Offline, tool-free cyber specialist benchmark with immutable evidence.
//!
//! This evaluates *supplied* responses only. It does not start a model, execute tools,
//! validate a network sandbox, or authorize cyber specialist deployment.

use crate::util::PathLexical as _;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util::{PyStr as _, canonical_json, casefold, sha256_hex};

/// Maximum characters in an authorization scope.
pub const MAX_SCOPE_CHARS: usize = 2_000;
/// Maximum characters in a case prompt.
pub const MAX_PROMPT_CHARS: usize = 8_000;
/// Maximum cases in a suite.
pub const MAX_CASES: usize = 64;
/// Maximum characters in one supplied answer.
pub const MAX_ANSWER_CHARS: usize = 16_000;

/// An evaluation violated its frozen contract or durable evidence rules.
#[derive(Debug, thiserror::Error)]
pub enum EvaluationError {
    /// Contract violation with a fixed message.
    #[error("{0}")]
    Contract(&'static str),
    /// Filesystem failure (including an already existing evidence record).
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON serialisation failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

/// One benchmark case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Case {
    /// Case slug.
    pub id: String,
    /// Prompt shown to the specialist.
    pub prompt: String,
    /// Terms every answer must contain.
    pub expected_terms: Vec<String>,
    /// Terms no answer may contain.
    #[serde(default)]
    pub forbidden_terms: Vec<String>,
}

/// A frozen benchmark suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suite {
    /// Suite slug.
    pub id: String,
    /// Human-authorized scope text.
    pub authorized_scope: String,
    /// Cases in order.
    pub cases: Vec<Case>,
}

/// A previously obtained answer for one case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Case this answers.
    pub case_id: String,
    /// Response text.
    pub response: String,
    /// Tool-call evidence (must be empty).
    #[serde(default)]
    pub tool_calls: Vec<String>,
}

/// Score for one case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseScore {
    /// Case scored.
    pub case_id: String,
    /// True when nothing is missing and nothing forbidden was hit.
    pub passed: bool,
    /// Expected terms absent from the answer.
    pub missing_terms: Vec<String>,
    /// Forbidden terms present in the answer.
    pub forbidden_hits: Vec<String>,
}

/// Result of scoring a suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evaluation {
    /// Suite slug.
    pub suite_id: String,
    /// SHA-256 of the canonical suite.
    pub suite_sha256: String,
    /// Cases passed.
    pub passed: usize,
    /// Cases total.
    pub total: usize,
    /// Per-case scores.
    pub scores: Vec<CaseScore>,
    /// Always false: a fixture pass never authorizes deployment.
    #[serde(default)]
    pub deployment_authorized: bool,
}

/// Injected, tool-free fixture source; never an agent execution interface.
pub trait Observer {
    /// Return a previously obtained answer without side effects.
    fn observe(&self, case: &Case) -> Observation;
}

/// Canonical JSON plus a trailing newline, as bytes.
fn canonical<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, EvaluationError> {
    let mut text = canonical_json(value)?;
    text.push('\n');
    Ok(text.into_bytes())
}

fn is_slug(text: &str) -> bool {
    let mut chars = text.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok
        && text.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn validate(suite: &Suite) -> Result<(), EvaluationError> {
    if !is_slug(&suite.id) {
        return Err(EvaluationError::Contract("Invalid suite ID"));
    }
    if suite.authorized_scope.py_strip().is_empty()
        || suite.authorized_scope.chars().count() > MAX_SCOPE_CHARS
    {
        return Err(EvaluationError::Contract(
            "Nonempty bounded authorization scope required",
        ));
    }
    if !(1..=MAX_CASES).contains(&suite.cases.len()) {
        return Err(EvaluationError::Contract("Invalid case count"));
    }
    let mut ids: Vec<&str> = Vec::new();
    for case in &suite.cases {
        validate_case(case, &mut ids)?;
    }
    Ok(())
}

fn validate_case<'a>(case: &'a Case, ids: &mut Vec<&'a str>) -> Result<(), EvaluationError> {
    if !is_slug(&case.id) || ids.contains(&case.id.as_str()) {
        return Err(EvaluationError::Contract("Invalid or duplicate case ID"));
    }
    ids.push(&case.id);
    if case.prompt.py_strip().is_empty() || case.prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(EvaluationError::Contract("Invalid prompt"));
    }
    if case.expected_terms.is_empty() || case.expected_terms.iter().any(|t| t.py_strip().is_empty())
    {
        return Err(EvaluationError::Contract("Expected terms must be nonempty"));
    }
    if case.forbidden_terms.iter().any(|t| t.py_strip().is_empty()) {
        return Err(EvaluationError::Contract(
            "Forbidden terms must be nonempty",
        ));
    }
    Ok(())
}

fn index_observations<'a>(
    suite: &Suite,
    observations: &'a [Observation],
) -> Result<HashMap<&'a str, &'a Observation>, EvaluationError> {
    if observations.len() != suite.cases.len() {
        return Err(EvaluationError::Contract(
            "Exactly one observation per case required",
        ));
    }
    let mut by_id: HashMap<&str, &Observation> = HashMap::new();
    for observation in observations {
        if by_id.contains_key(observation.case_id.as_str()) {
            return Err(EvaluationError::Contract("Duplicate observation"));
        }
        if !observation.tool_calls.is_empty() {
            return Err(EvaluationError::Contract(
                "Tool invocation evidence is forbidden",
            ));
        }
        if observation.response.chars().count() > MAX_ANSWER_CHARS {
            return Err(EvaluationError::Contract("Invalid response"));
        }
        by_id.insert(&observation.case_id, observation);
    }
    if !suite
        .cases
        .iter()
        .all(|c| by_id.contains_key(c.id.as_str()))
    {
        return Err(EvaluationError::Contract(
            "Missing or unexpected observation",
        ));
    }
    Ok(by_id)
}

fn score_case(case: &Case, response: &str) -> CaseScore {
    let answer = casefold(response);
    let missing: Vec<String> = case
        .expected_terms
        .iter()
        .filter(|term| !answer.contains(&casefold(term)))
        .cloned()
        .collect();
    let forbidden: Vec<String> = case
        .forbidden_terms
        .iter()
        .filter(|term| answer.contains(&casefold(term)))
        .cloned()
        .collect();
    CaseScore {
        case_id: case.id.clone(),
        passed: missing.is_empty() && forbidden.is_empty(),
        missing_terms: missing,
        forbidden_hits: forbidden,
    }
}

/// Deterministically score pre-collected, tool-free answers, failing closed.
///
/// # Errors
/// Evaluation inputs or evidence are invalid, or evaluation data cannot be read, encoded, or
/// persisted.
pub fn evaluate_offline(
    suite: &Suite,
    observations: &[Observation],
) -> Result<Evaluation, EvaluationError> {
    validate(suite)?;
    let by_id = index_observations(suite, observations)?;
    let scores: Vec<CaseScore> = suite
        .cases
        .iter()
        .map(|case| {
            score_case(
                case,
                by_id.get(case.id.as_str()).map_or("", |o| &o.response),
            )
        })
        .collect();
    Ok(Evaluation {
        suite_id: suite.id.clone(),
        suite_sha256: sha256_hex(&canonical(suite)?),
        passed: scores.iter().filter(|s| s.passed).count(),
        total: scores.len(),
        scores,
        deployment_authorized: false,
    })
}

#[cfg(unix)]
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW;
    let fd = rustix::fs::open(path, flags, Mode::from_raw_mode(0o600))?;
    Ok(File::from(fd))
}

#[cfg(not(unix))]
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Store an exclusive private evaluation record; refuse overwrite and symlinks.
///
/// # Errors
/// Evaluation inputs or evidence are invalid, or evaluation data cannot be read, encoded, or
/// persisted.
pub fn persist_evaluation(
    directory: &Path,
    suite: &Suite,
    result: &Evaluation,
) -> Result<PathBuf, EvaluationError> {
    validate(suite)?;
    if result.suite_id != suite.id || result.suite_sha256 != sha256_hex(&canonical(suite)?) {
        return Err(EvaluationError::Contract("Suite digest mismatch"));
    }
    if !directory.is_dir() || directory.is_symlink_lexical() {
        return Err(EvaluationError::Contract(
            "Evidence directory must exist and not be a symlink",
        ));
    }
    let path = directory.join(format!("{}-{}.json", suite.id, result.suite_sha256));
    let mut stream = open_exclusive(&path)?;
    let written = canonical(result).and_then(|bytes| {
        stream.write_all(&bytes)?;
        stream.flush()?;
        stream.sync_all()?;
        Ok(())
    });
    if let Err(err) = written {
        let _ = std::fs::remove_file(&path);
        return Err(err);
    }
    Ok(path)
}
