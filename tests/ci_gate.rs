#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! CI assessment tests use synthetic API returns; no GitHub requests.

use hermes_harness::ci::ci_gate::assess_github_ci;
use hermes_harness::ci::ci_observation::CIPolicy;
use hermes_harness::ci::ci_request::CIRequest;

const RUN_ID: i64 = 42;
const TOKEN: &str = "synthetic-test-token";

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

#[test]
fn wrong_run_id_rejected_without_fetch() {
    let err = assess_github_ci(&candidate(), &policy(), 0, TOKEN).unwrap_err();
    assert!(
        err.to_string().contains("positive GitHub Actions run ID"),
        "{err}"
    );
}

#[test]
fn no_token_rejected() {
    let err = assess_github_ci(&candidate(), &policy(), RUN_ID, "").unwrap_err();
    assert!(err.to_string().contains("token is required"), "{err}");
}
