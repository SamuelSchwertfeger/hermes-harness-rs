//! Offline validation of GitHub Actions observations for immutable CI candidates.
//!
//! The observations must originate from a separately authenticated GitHub API client.
//! This module does not fetch data, authenticate its provenance, or authorize merge.

use std::collections::BTreeSet;

use crate::ci::CiError;
use crate::ci::ci_request::CIRequest;

/// Outcome of evaluating untrusted observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CIEvaluation {
    /// Everything required succeeded (still untrusted).
    PassingObservation,
    /// Not yet decidable.
    Incomplete,
    /// Mismatch or failure.
    Rejected,
}

impl std::fmt::Display for CIEvaluation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl CIEvaluation {
    /// The Python enum value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PassingObservation => "passing_observation_untrusted",
            Self::Incomplete => "incomplete",
            Self::Rejected => "rejected",
        }
    }
}

/// One observed workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitHubRunObservation {
    /// `owner/name`.
    pub repository: String,
    /// Workflow file path.
    pub workflow_path: String,
    /// Commit the run executed.
    pub head_sha: String,
    /// Trigger event.
    pub event: String,
    /// Run identifier.
    pub run_id: i64,
    /// Run attempt number.
    pub run_attempt: i64,
    /// Run status.
    pub status: String,
    /// Run conclusion, when finished.
    pub conclusion: Option<String>,
}

/// One observed job of a run attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitHubJobObservation {
    /// Run identifier.
    pub run_id: i64,
    /// Run attempt number.
    pub run_attempt: i64,
    /// Job name.
    pub name: String,
    /// Job status.
    pub status: String,
    /// Job conclusion, when finished.
    pub conclusion: Option<String>,
}

/// A run with all its jobs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GitHubEvidence {
    /// The run.
    pub run: GitHubRunObservation,
    /// Its jobs.
    pub jobs: Vec<GitHubJobObservation>,
}

/// Expected repository, workflow, jobs and triggers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CIPolicy {
    /// `owner/name`.
    pub repository: String,
    /// Workflow file path.
    pub workflow_path: String,
    /// Jobs that must all succeed.
    pub required_jobs: Vec<String>,
    /// Accepted trigger events.
    pub allowed_events: Vec<String>,
}

impl CIPolicy {
    /// Policy with the default allowed events (`push`, `pull_request`).
    #[must_use]
    pub fn new(repository: &str, workflow_path: &str, required_jobs: &[&str]) -> Self {
        Self {
            repository: repository.to_owned(),
            workflow_path: workflow_path.to_owned(),
            required_jobs: required_jobs.iter().map(|job| (*job).to_owned()).collect(),
            allowed_events: vec!["push".to_owned(), "pull_request".to_owned()],
        }
    }
}

fn fail<T>(msg: &str) -> Result<T, CiError> {
    Err(CiError::Message(msg.to_owned()))
}

/// Reject malformed policies.
fn validate_policy(policy: &CIPolicy) -> Result<(), CiError> {
    if policy.repository.is_empty() || policy.repository.matches('/').count() != 1 {
        return fail("CI repository must be owner/name");
    }
    if !policy.workflow_path.starts_with(".github/workflows/")
        || policy.workflow_path.contains("..")
    {
        return fail("Invalid workflow path");
    }
    let unique: BTreeSet<&String> = policy.required_jobs.iter().collect();
    if policy.required_jobs.is_empty() || policy.required_jobs.len() != unique.len() {
        return fail("Required CI jobs must be nonempty and unique");
    }
    if policy.allowed_events.is_empty() {
        return fail("At least one CI trigger is required");
    }
    Ok(())
}

/// Reject mismatched run identities before checking completion.
#[must_use]
fn evaluate_run(
    candidate: &CIRequest,
    run: &GitHubRunObservation,
    policy: &CIPolicy,
) -> Option<CIEvaluation> {
    let commit_matches = run.head_sha == candidate.commit;
    let identity_matches = run.repository == policy.repository
        && run.workflow_path == policy.workflow_path
        && commit_matches;
    if !identity_matches {
        return Some(CIEvaluation::Rejected);
    }
    if !policy.allowed_events.contains(&run.event) || run.run_id <= 0 || run.run_attempt <= 0 {
        return Some(CIEvaluation::Rejected);
    }
    if run.status != "completed" {
        return Some(CIEvaluation::Incomplete);
    }
    if run.conclusion.as_deref() != Some("success") {
        return Some(CIEvaluation::Rejected);
    }
    None
}

/// Validate all provided job records against the selected run attempt.
#[must_use]
fn evaluate_jobs(evidence: &GitHubEvidence, policy: &CIPolicy) -> CIEvaluation {
    if evidence.jobs.is_empty() {
        return CIEvaluation::Incomplete;
    }
    let mut names: BTreeSet<&str> = BTreeSet::new();
    let mut incomplete = false;
    for job in &evidence.jobs {
        let identity_matches = job.run_id == evidence.run.run_id
            && job.run_attempt == evidence.run.run_attempt
            && !job.name.is_empty()
            && !names.contains(job.name.as_str());
        if !identity_matches {
            return CIEvaluation::Rejected;
        }
        names.insert(&job.name);
        if job.status != "completed" {
            incomplete = true;
        } else if job.conclusion.as_deref() != Some("success") {
            return CIEvaluation::Rejected;
        }
    }
    if incomplete
        || !policy
            .required_jobs
            .iter()
            .all(|job| names.contains(job.as_str()))
    {
        return CIEvaluation::Incomplete;
    }
    CIEvaluation::PassingObservation
}

/// Evaluate untrusted observations; never assert authenticated provenance.
///
/// Caller must independently fetch the current run and all paginated jobs
/// from GitHub through an authenticated TLS channel and verify run attempts.
///
/// # Errors
/// Returns a [`CiError`] when the policy is malformed.
pub fn evaluate_ci_observation(
    candidate: &CIRequest,
    evidence: &GitHubEvidence,
    policy: &CIPolicy,
) -> Result<CIEvaluation, CiError> {
    validate_policy(policy)?;
    if let Some(result) = evaluate_run(candidate, &evidence.run, policy) {
        return Ok(result);
    }
    Ok(evaluate_jobs(evidence, policy))
}
