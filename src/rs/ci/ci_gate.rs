//! Read-only, decision-time GitHub CI assessment for an immutable candidate.
//!
//! The gate makes fresh GitHub REST reads using the fixed-origin client. A positive
//! assessment is not a human approval, merge permission, or signed attestation.

use crate::ci::CiError;
use crate::ci::ci_observation::{CIEvaluation, CIPolicy, evaluate_ci_observation};
use crate::ci::ci_request::CIRequest;
use crate::ci::github_ci_reader::{GitHubReadClient, JsonReader, fetch_github_ci_evidence};

/// Read-only status; no value conveys permission to mutate anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CIGateStatus {
    /// CI observed passing; not an approval.
    ObservedPass,
    /// Not yet decidable.
    Incomplete,
    /// Mismatch or failure.
    Rejected,
    /// Transport or evidence failure.
    Unknown,
}

impl std::fmt::Display for CIGateStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl CIGateStatus {
    /// The Python enum value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ObservedPass => "observed_pass_not_approval",
            Self::Incomplete => "incomplete",
            Self::Rejected => "rejected",
            Self::Unknown => "unknown",
        }
    }
}

/// Scoped result of fresh GitHub observation, without secrets or raw JSON.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CIGateAssessment {
    /// Assessment status.
    pub status: CIGateStatus,
    /// Candidate commit.
    pub commit: String,
    /// Candidate tree.
    pub tree: String,
    /// GitHub Actions run identifier.
    pub run_id: i64,
    /// Observed run attempt, when evidence was obtained.
    pub run_attempt: Option<i64>,
    /// Repository name.
    pub repository: String,
    /// Workflow path.
    pub workflow_path: String,
}

/// Re-fetch GitHub state and evaluate it without approvals or writes.
///
/// Transport failures return UNKNOWN. Invalid/mismatched API evidence fails
/// closed. This boundary builds its own GitHub reader; the internal test seam
/// accepts a caller-supplied reader.
///
/// # Errors
/// Returns a [`CiError`] when the token is empty or `run_id` is not positive.
pub fn assess_github_ci(
    candidate: &CIRequest,
    policy: &CIPolicy,
    run_id: i64,
    github_token: &str,
) -> Result<CIGateAssessment, CiError> {
    assess_with_reader(
        candidate,
        policy,
        run_id,
        github_token,
        &GitHubReadClient::new(github_token),
    )
}

/// Evaluate the gate with a caller-supplied reader; a nonempty token is still required.
///
/// # Errors
/// Same as [`assess_github_ci`].
pub(crate) fn assess_with_reader(
    candidate: &CIRequest,
    policy: &CIPolicy,
    run_id: i64,
    github_token: &str,
    reader: &dyn JsonReader,
) -> Result<CIGateAssessment, CiError> {
    if github_token.is_empty() {
        return Err(CiError::Message(
            "A GitHub read-only token is required for a fresh CI assessment".to_owned(),
        ));
    }
    if run_id <= 0 {
        return Err(CiError::Message(
            "A positive GitHub Actions run ID is required".to_owned(),
        ));
    }
    let outcome =
        fetch_github_ci_evidence(candidate, policy, run_id, reader).and_then(|evidence| {
            let evaluation = evaluate_ci_observation(candidate, &evidence, policy)?;
            Ok((evidence, evaluation))
        });
    let assessment = |status, run_attempt| CIGateAssessment {
        status,
        commit: candidate.commit.clone(),
        tree: candidate.tree.clone(),
        run_id,
        run_attempt,
        repository: policy.repository.clone(),
        workflow_path: policy.workflow_path.clone(),
    };
    Ok(match outcome {
        Err(_) => {
            log::warn!("GitHub CI assessment unavailable or invalid for run {run_id}");
            assessment(CIGateStatus::Unknown, None)
        }
        Ok((evidence, evaluation)) => {
            let status = match evaluation {
                CIEvaluation::PassingObservation => CIGateStatus::ObservedPass,
                CIEvaluation::Incomplete => CIGateStatus::Incomplete,
                CIEvaluation::Rejected => CIGateStatus::Rejected,
            };
            assessment(status, Some(evidence.run.run_attempt))
        }
    })
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
    use std::collections::BTreeMap;

    use crate::ci::CiError;
    use crate::ci::ci_gate::{CIGateStatus, assess_with_reader};
    use crate::ci::ci_observation::CIPolicy;
    use crate::ci::ci_request::CIRequest;
    use serde_json::{Value, json};

    const RUN_ID: i64 = 42;
    const ATTEMPT: i64 = 2;
    const TOKEN: &str = "synthetic-test-token";

    type Routes = BTreeMap<String, Value>;

    fn commit() -> String {
        "a".repeat(40)
    }

    fn candidate() -> CIRequest {
        CIRequest {
            task_id: "task1".to_owned(),
            brief_digest: "hash".to_owned(),
            baseline: "c".repeat(40),
            commit: commit(),
            tree: "b".repeat(40),
            branch: "harness/task1".to_owned(),
            changed_paths: vec!["x.py".to_owned()],
            content_sha256: "digest".to_owned(),
        }
    }

    fn policy() -> CIPolicy {
        CIPolicy::new("owner/repo", ".github/workflows/ci.yml", &["unit"])
    }

    fn run_key() -> String {
        format!("/repos/owner/repo/actions/runs/{RUN_ID}")
    }

    fn commit_key() -> String {
        format!("/repos/owner/repo/git/commits/{}", commit())
    }

    fn jobs_key() -> String {
        format!(
            "/repos/owner/repo/actions/runs/{RUN_ID}/attempts/{ATTEMPT}/jobs?per_page=100&page=1"
        )
    }

    fn routes(candidate: &CIRequest) -> Routes {
        [
            (
                run_key(),
                json!({
                    "repository": {"full_name": "owner/repo"},
                    "path": "owner/repo/.github/workflows/ci.yml@refs/heads/main",
                    "head_sha": candidate.commit,
                    "id": RUN_ID,
                    "run_attempt": ATTEMPT,
                    "event": "push",
                    "status": "completed",
                    "conclusion": "success",
                }),
            ),
            (
                commit_key(),
                json!({"sha": candidate.commit, "tree": {"sha": candidate.tree}}),
            ),
            (
                jobs_key(),
                json!({
                    "total_count": 1,
                    "jobs": [{
                        "id": 100,
                        "run_id": RUN_ID,
                        "run_attempt": ATTEMPT,
                        "name": "unit",
                        "status": "completed",
                        "conclusion": "success",
                    }],
                }),
            ),
        ]
        .into_iter()
        .collect()
    }

    fn assess(candidate: &CIRequest, routes: &Routes) -> crate::ci::ci_gate::CIGateAssessment {
        let reader = |path: &str| {
            routes
                .get(path)
                .cloned()
                .ok_or_else(|| CiError::Message("KeyError".to_owned()))
        };
        assess_with_reader(candidate, &policy(), RUN_ID, TOKEN, &reader).unwrap()
    }

    #[test]
    fn fresh_read_returns_non_authorizing_pass() {
        let candidate = candidate();
        let result = assess(&candidate, &routes(&candidate));
        assert_eq!(result.status, CIGateStatus::ObservedPass);
        assert_eq!(result.run_attempt, Some(ATTEMPT));
        assert_eq!(result.tree, candidate.tree);
    }

    #[test]
    fn incomplete_is_not_pass() {
        let candidate = candidate();
        let mut routes = routes(&candidate);
        let run = routes.get_mut(&run_key()).unwrap();
        run["status"] = json!("in_progress");
        run["conclusion"] = Value::Null;
        assert_eq!(assess(&candidate, &routes).status, CIGateStatus::Incomplete);
    }

    #[test]
    fn mismatched_tree_is_unknown() {
        let candidate = candidate();
        let mut routes = routes(&candidate);
        routes.get_mut(&commit_key()).unwrap()["tree"] = json!({"sha": "0".repeat(40)});
        assert_eq!(assess(&candidate, &routes).status, CIGateStatus::Unknown);
    }

    #[test]
    fn transport_failure_is_unknown() {
        let candidate = candidate();
        let reader = |_path: &str| Err(CiError::Message("synthetic API failure".to_owned()));
        let result = assess_with_reader(&candidate, &policy(), RUN_ID, TOKEN, &reader).unwrap();
        assert_eq!(result.status, CIGateStatus::Unknown);
        assert_eq!(result.run_attempt, None);
    }

    #[test]
    fn unsuccessful_job_is_rejected() {
        let candidate = candidate();
        let mut routes = routes(&candidate);
        routes.get_mut(&jobs_key()).unwrap()["jobs"][0]["conclusion"] = json!("failure");
        assert_eq!(assess(&candidate, &routes).status, CIGateStatus::Rejected);
    }
}
