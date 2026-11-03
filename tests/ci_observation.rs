#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Offline CI observation contract; no network or credentials.

use hermes_harness::ci::ci_observation::{
    CIEvaluation, CIPolicy, GitHubEvidence, GitHubJobObservation, GitHubRunObservation,
    evaluate_ci_observation,
};
use hermes_harness::ci::ci_request::CIRequest;

const QUALITY: &str = "Quality (Python 3.14)";

fn case() -> (
    CIRequest,
    GitHubRunObservation,
    Vec<GitHubJobObservation>,
    CIPolicy,
) {
    let commit = "c".repeat(40);
    let candidate = CIRequest {
        task_id: "t".to_owned(),
        brief_digest: "d".to_owned(),
        baseline: "b".repeat(40),
        commit: commit.clone(),
        tree: "e".repeat(40),
        branch: "harness/t".to_owned(),
        changed_paths: vec![],
        content_sha256: "x".to_owned(),
    };
    let run = GitHubRunObservation {
        repository: "example/harness".to_owned(),
        workflow_path: ".github/workflows/ci.yml".to_owned(),
        head_sha: commit,
        event: "pull_request".to_owned(),
        run_id: 123,
        run_attempt: 2,
        status: "completed".to_owned(),
        conclusion: Some("success".to_owned()),
    };
    let jobs = vec![GitHubJobObservation {
        run_id: 123,
        run_attempt: 2,
        name: QUALITY.to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("success".to_owned()),
    }];
    let policy = CIPolicy::new("example/harness", ".github/workflows/ci.yml", &[QUALITY]);
    (candidate, run, jobs, policy)
}

fn evidence(run: GitHubRunObservation, jobs: Vec<GitHubJobObservation>) -> GitHubEvidence {
    GitHubEvidence { run, jobs }
}

#[test]
fn success_is_only_untrusted_observation() {
    let (candidate, run, jobs, policy) = case();
    assert_eq!(
        evaluate_ci_observation(&candidate, &evidence(run, jobs), &policy).unwrap(),
        CIEvaluation::PassingObservation
    );
}

#[test]
fn mismatched_run_rejected() {
    for field in [
        "repository",
        "workflow_path",
        "head_sha",
        "event",
        "run_attempt",
        "conclusion",
    ] {
        let (candidate, mut run, jobs, policy) = case();
        match field {
            "repository" => run.repository = "other/fork".to_owned(),
            "workflow_path" => run.workflow_path = ".github/workflows/other.yml".to_owned(),
            "head_sha" => run.head_sha = "0".repeat(40),
            "event" => run.event = "workflow_dispatch".to_owned(),
            "run_attempt" => run.run_attempt = 0,
            _ => run.conclusion = Some("failure".to_owned()),
        }
        assert_eq!(
            evaluate_ci_observation(&candidate, &evidence(run, jobs), &policy).unwrap(),
            CIEvaluation::Rejected,
            "{field}"
        );
    }
}

#[test]
fn unfinished_run_is_incomplete() {
    let (candidate, mut run, jobs, policy) = case();
    run.status = "in_progress".to_owned();
    assert_eq!(
        evaluate_ci_observation(&candidate, &evidence(run, jobs), &policy).unwrap(),
        CIEvaluation::Incomplete
    );
}

#[test]
fn job_attempt_mismatch_rejected() {
    let (candidate, run, mut jobs, policy) = case();
    jobs[0].run_attempt = 1;
    assert_eq!(
        evaluate_ci_observation(&candidate, &evidence(run, jobs), &policy).unwrap(),
        CIEvaluation::Rejected
    );
}

#[test]
fn missing_required_job_incomplete() {
    let (candidate, run, jobs, mut policy) = case();
    policy.required_jobs = vec![QUALITY.to_owned(), "Security".to_owned()];
    assert_eq!(
        evaluate_ci_observation(&candidate, &evidence(run, jobs), &policy).unwrap(),
        CIEvaluation::Incomplete
    );
}

#[test]
fn duplicate_jobs_rejected() {
    let (candidate, run, jobs, policy) = case();
    let doubled = [jobs.clone(), jobs].concat();
    assert_eq!(
        evaluate_ci_observation(&candidate, &evidence(run, doubled), &policy).unwrap(),
        CIEvaluation::Rejected
    );
}

#[test]
fn empty_policy_fail_closed() {
    let (candidate, run, jobs, mut policy) = case();
    policy.required_jobs = vec![];
    let err = evaluate_ci_observation(&candidate, &evidence(run, jobs), &policy).unwrap_err();
    assert!(err.to_string().contains("nonempty"), "{err}");
}
