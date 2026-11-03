//! Read-only GitHub Actions REST observation retrieval for offline CI candidates.
//!
//! Only an operator-controlled, authenticated transport can provide trustworthy origin.
//! A returned observation is not an attestation, gate, or approval decision.

use std::collections::BTreeSet;
use std::io::Read as _;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::ci::CiError;
use crate::ci::ci_observation::{
    CIPolicy, GitHubEvidence, GitHubJobObservation, GitHubRunObservation,
};
use crate::ci::ci_request::CIRequest;

const API: &str = "https://api.github.com";
const MAX_BYTES: usize = 2_000_000;
const MAX_PAGES: usize = 20;
const PAGE_SIZE: usize = 100;
const MAX_TIMEOUT: f64 = 30.0;

type Res<T> = Result<T, CiError>;

fn fail<T>(msg: &str) -> Res<T> {
    Err(CiError::Message(msg.to_owned()))
}

/// Transport cause whose formatting cannot expose authorization header values.
pub struct GitHubTransportError(ureq::Error);

impl std::fmt::Debug for GitHubTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHubTransportError(<redacted>)")
    }
}

impl std::fmt::Display for GitHubTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHub API read failed; CI state is unknown")
    }
}

impl std::error::Error for GitHubTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Injection seam; implement with authenticated HTTPS for real observations.
pub trait JsonReader {
    /// Fetch one allowlisted API path and return the decoded JSON value.
    ///
    /// # Errors
    /// Returns a [`CiError`] when the read fails or the response is invalid.
    fn read(&self, path: &str) -> Res<Value>;
}

impl<F> JsonReader for F
where
    F: Fn(&str) -> Res<Value>,
{
    fn read(&self, path: &str) -> Res<Value> {
        self(path)
    }
}

fn name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')
}

fn is_name(text: &str) -> bool {
    !text.is_empty() && text.chars().all(name_char)
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit())
}

fn is_sha(text: &str) -> bool {
    text.len() == 40
        && text
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
}

fn is_repository(text: &str) -> bool {
    text.split_once('/')
        .is_some_and(|(owner, name)| is_name(owner) && is_name(name))
}

/// Hand-written equivalent of the Python `_ALLOWED_PATH` full-match regex.
#[must_use]
fn allowed_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/repos/") else {
        return false;
    };
    let parts: Vec<&str> = rest.splitn(3, '/').collect();
    let [owner, name, tail] = parts.as_slice() else {
        return false;
    };
    if !is_name(owner) || !is_name(name) {
        return false;
    }
    if let Some(sha) = tail.strip_prefix("git/commits/") {
        return is_sha(sha);
    }
    let Some(run) = tail.strip_prefix("actions/runs/") else {
        return false;
    };
    let Some((run_id, jobs)) = run.split_once("/attempts/") else {
        return is_digits(run);
    };
    let Some((attempt, page)) = jobs.split_once("/jobs?per_page=100&page=") else {
        return false;
    };
    is_digits(run_id) && is_digits(attempt) && is_digits(page)
}

/// GET-only fixed-origin client; token never enters URLs or logs.
#[derive(Clone, PartialEq)]
pub struct GitHubReadClient {
    /// Bearer token.
    pub token: String,
    /// Request timeout in seconds (must be in `(0, 30]`).
    pub timeout_seconds: f64,
}

impl std::fmt::Debug for GitHubReadClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubReadClient")
            .field("token", &"<redacted>")
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

impl GitHubReadClient {
    /// Client with the default 10 second timeout.
    #[must_use]
    pub fn new(token: &str) -> Self {
        Self {
            token: token.to_owned(),
            timeout_seconds: 10.0,
        }
    }

    fn fetch_bytes(&self, path: &str) -> Res<Vec<u8>> {
        let warn_read_failure = || log::warn!("GitHub read failed for a bounded API path");
        let unavailable =
            || CiError::Message("GitHub API read failed; CI state is unknown".to_owned());
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs_f64(self.timeout_seconds)))
            .build()
            .into();
        let response = agent
            .get(&format!("{API}{path}"))
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "Hermes-Harness-ReadOnly")
            .call()
            .map_err(|error| {
                warn_read_failure();
                match error {
                    ureq::Error::StatusCode(_) => unavailable(),
                    source => CiError::GitHubTransport(Box::new(GitHubTransportError(source))),
                }
            })?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            warn_read_failure();
            return Err(unavailable());
        }
        if status != 200 {
            return fail("GitHub returned an unexpected status");
        }
        let mut data = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|error| {
                warn_read_failure();
                CiError::GitHubRead(error)
            })?;
        if data.len() > MAX_BYTES {
            return fail("GitHub response exceeds size bound");
        }
        Ok(data)
    }
}

impl JsonReader for GitHubReadClient {
    fn read(&self, path: &str) -> Res<Value> {
        if self.token.is_empty()
            || !(self.timeout_seconds > 0.0 && self.timeout_seconds <= MAX_TIMEOUT)
        {
            return fail("GitHub reader needs a token and bounded timeout");
        }
        if !allowed_path(path) {
            return fail("GitHub REST path is outside supported read-only scope");
        }
        let data = self.fetch_bytes(path)?;
        let decoded = serde_json::from_slice::<Value>(&data).map_err(CiError::GitHubJson)?;
        object(Some(&decoded))?;
        Ok(decoded)
    }
}

fn object(value: Option<&Value>) -> Res<&Map<String, Value>> {
    value
        .and_then(Value::as_object)
        .map_or_else(|| fail("Expected JSON object from GitHub"), Ok)
}

fn string(value: Option<&Value>) -> Res<String> {
    value.and_then(Value::as_str).map_or_else(
        || fail("Invalid GitHub string field"),
        |text| Ok(text.to_owned()),
    )
}

fn positive(value: Option<&Value>) -> Res<i64> {
    match value.and_then(Value::as_i64) {
        Some(number) if number > 0 => Ok(number),
        _ => fail("Invalid GitHub numeric identifier"),
    }
}

fn conclusion(value: Option<&Value>) -> Res<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        other => string(other).map(Some),
    }
}

fn validate_request(candidate: &CIRequest, policy: &CIPolicy, run_id: i64) -> Res<String> {
    if !is_repository(&policy.repository)
        || policy
            .repository
            .split('/')
            .any(|part| part == "." || part == "..")
        || !is_sha(&candidate.commit)
    {
        return fail("Invalid requested repository or commit");
    }
    if !policy.workflow_path.starts_with(".github/workflows/")
        || policy.workflow_path.contains("..")
    {
        return fail("Invalid requested workflow path");
    }
    positive(Some(&Value::from(run_id)))?;
    Ok(format!("/repos/{}", policy.repository))
}

fn workflow_path(raw: Option<&Value>, policy: &CIPolicy) -> Res<String> {
    let path = string(raw)?;
    // GitHub returns e.g. owner/repo/.github/workflows/ci.yml@refs/heads/main.
    let prefix = format!("{}/{}@", policy.repository, policy.workflow_path);
    if !path.starts_with(&prefix) || path.chars().count() <= prefix.chars().count() {
        return fail("Workflow run does not match the requested workflow");
    }
    Ok(policy.workflow_path.clone())
}

fn read_run(
    fetch: &dyn JsonReader,
    base: &str,
    run_id: i64,
    policy: &CIPolicy,
    candidate: &CIRequest,
) -> Res<GitHubRunObservation> {
    let fetched = fetch.read(&format!("{base}/actions/runs/{run_id}"))?;
    let payload = object(Some(&fetched))?;
    let repo = object(payload.get("repository"))?;
    let repository = string(repo.get("full_name"))?;
    let workflow = workflow_path(payload.get("path"), policy)?;
    let head_sha = string(payload.get("head_sha"))?;
    let actual_id = positive(payload.get("id"))?;
    let attempt = positive(payload.get("run_attempt"))?;
    if repository != policy.repository || head_sha != candidate.commit || actual_id != run_id {
        return fail("GitHub run identity does not match candidate");
    }
    Ok(GitHubRunObservation {
        repository,
        workflow_path: workflow,
        head_sha,
        event: string(payload.get("event"))?,
        run_id: actual_id,
        run_attempt: attempt,
        status: string(payload.get("status"))?,
        conclusion: conclusion(payload.get("conclusion"))?,
    })
}

fn verify_tree(fetch: &dyn JsonReader, base: &str, candidate: &CIRequest) -> Res<()> {
    let fetched = fetch.read(&format!("{base}/git/commits/{}", candidate.commit))?;
    let commit = object(Some(&fetched))?;
    let tree = object(commit.get("tree"))?;
    if string(commit.get("sha"))? != candidate.commit || string(tree.get("sha"))? != candidate.tree
    {
        return fail("GitHub commit tree does not match immutable CI candidate");
    }
    Ok(())
}

fn job_total(value: Option<&Value>) -> Res<usize> {
    match value.and_then(Value::as_i64).map(usize::try_from) {
        Some(Ok(total)) if total <= PAGE_SIZE * MAX_PAGES => Ok(total),
        _ => fail("Unbounded or malformed GitHub job count"),
    }
}

fn job_batch(payload: &Map<String, Value>, total: usize, prior: usize) -> Res<&Vec<Value>> {
    if job_total(payload.get("total_count"))? != total {
        return fail("GitHub job pagination changed or is invalid");
    }
    let Some(batch) = payload.get("jobs").and_then(Value::as_array) else {
        return fail("GitHub job pagination changed or is invalid");
    };
    if batch.len() > PAGE_SIZE || prior + batch.len() > total {
        return fail("GitHub job pagination exceeds reported count");
    }
    Ok(batch)
}

/// Python `value == attempt` for a JSON value (bool and float compare numerically).
fn equals_int(value: &Value, expected: i64) -> bool {
    match value {
        Value::Bool(flag) => i64::from(*flag) == expected,
        Value::Number(number) => number.as_i64().map_or_else(
            || {
                let wanted = i32::try_from(expected).ok().map(f64::from);
                number
                    .as_f64()
                    .zip(wanted)
                    .is_some_and(|(float, want)| (float - want).abs() < f64::EPSILON)
            },
            |int| int == expected,
        ),
        _ => false,
    }
}

fn parse_job(
    entry: &Value,
    run: &GitHubRunObservation,
    seen_ids: &mut BTreeSet<i64>,
) -> Res<GitHubJobObservation> {
    let job = object(Some(entry))?;
    let job_id = positive(job.get("id"))?;
    if seen_ids.contains(&job_id) || positive(job.get("run_id"))? != run.run_id {
        return fail("Duplicate or mismatched GitHub job identity");
    }
    if job
        .get("run_attempt")
        .is_some_and(|attempt| !equals_int(attempt, run.run_attempt))
    {
        return fail("Duplicate or mismatched GitHub job identity");
    }
    seen_ids.insert(job_id);
    Ok(GitHubJobObservation {
        run_id: run.run_id,
        run_attempt: run.run_attempt,
        name: string(job.get("name"))?,
        status: string(job.get("status"))?,
        conclusion: conclusion(job.get("conclusion"))?,
    })
}

fn read_jobs(
    fetch: &dyn JsonReader,
    base: &str,
    run: &GitHubRunObservation,
) -> Res<Vec<GitHubJobObservation>> {
    let mut jobs: Vec<GitHubJobObservation> = Vec::new();
    let mut seen_ids: BTreeSet<i64> = BTreeSet::new();
    let mut total: Option<usize> = None;
    for page in 1..=MAX_PAGES {
        let path = format!(
            "{base}/actions/runs/{}/attempts/{}/jobs?per_page={PAGE_SIZE}&page={page}",
            run.run_id, run.run_attempt
        );
        let fetched = fetch.read(&path)?;
        let payload = object(Some(&fetched))?;
        let expected = if let Some(known) = total {
            known
        } else {
            let first = job_total(payload.get("total_count"))?;
            total = Some(first);
            first
        };
        let batch = job_batch(payload, expected, jobs.len())?;
        for entry in batch {
            jobs.push(parse_job(entry, run, &mut seen_ids)?);
        }
        if jobs.len() == expected {
            return Ok(jobs);
        }
        if batch.is_empty() {
            break;
        }
    }
    fail("GitHub job pagination incomplete; no CI conclusion can be inferred")
}

/// Fetch exact run, Git commit tree, and all jobs of its latest attempt.
///
/// A caller-injected reader is for deterministic tests; injection alone supplies
/// no provenance. Do not grant approval based on these returned values.
///
/// # Errors
/// Returns a [`CiError`] for invalid request inputs, reader failures,
/// mismatched run or commit identity, and malformed or incomplete job pages.
pub fn fetch_github_ci_evidence(
    candidate: &CIRequest,
    policy: &CIPolicy,
    run_id: i64,
    reader: &dyn JsonReader,
) -> Res<GitHubEvidence> {
    let base = validate_request(candidate, policy, run_id)?;
    let run = read_run(reader, &base, run_id, policy, candidate)?;
    verify_tree(reader, &base, candidate)?;
    let jobs = read_jobs(reader, &base, &run)?;
    Ok(GitHubEvidence { run, jobs })
}
