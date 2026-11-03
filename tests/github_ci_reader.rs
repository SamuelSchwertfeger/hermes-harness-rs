#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Read-only GitHub API reader tests with injected responses; no network.

use std::cell::RefCell;
use std::collections::BTreeMap;

use hermes_harness::ci::CiError;
use hermes_harness::ci::ci_observation::{CIEvaluation, CIPolicy, evaluate_ci_observation};
use hermes_harness::ci::ci_request::CIRequest;
use hermes_harness::ci::github_ci_reader::fetch_github_ci_evidence;
use serde_json::{Value, json};

type Routes = BTreeMap<String, Value>;

const ROOT: &str = "/repos/owner/repo";

fn commit() -> String {
    "a".repeat(40)
}

fn tree() -> String {
    "b".repeat(40)
}

fn inputs() -> (CIRequest, CIPolicy, Routes) {
    let candidate = CIRequest {
        task_id: "t".to_owned(),
        brief_digest: "digest".to_owned(),
        baseline: "c".repeat(40),
        commit: commit(),
        tree: tree(),
        branch: "harness/t".to_owned(),
        changed_paths: vec!["src/x.py".to_owned()],
        content_sha256: "digest".to_owned(),
    };
    let policy = CIPolicy::new("owner/repo", ".github/workflows/ci.yml", &["quality"]);
    let run = json!({
        "id": 45,
        "run_attempt": 2,
        "path": "owner/repo/.github/workflows/ci.yml@refs/heads/main",
        "repository": {"full_name": "owner/repo"},
        "head_sha": commit(),
        "event": "push",
        "status": "completed",
        "conclusion": "success",
    });
    let jobs = json!({
        "total_count": 1,
        "jobs": [{
            "id": 55,
            "run_id": 45,
            "run_attempt": 2,
            "name": "quality",
            "status": "completed",
            "conclusion": "success",
        }],
    });
    let routes: Routes = [
        (format!("{ROOT}/actions/runs/45"), run),
        (
            format!("{ROOT}/git/commits/{}", commit()),
            json!({"sha": commit(), "tree": {"sha": tree()}}),
        ),
        (
            format!("{ROOT}/actions/runs/45/attempts/2/jobs?per_page=100&page=1"),
            jobs,
        ),
    ]
    .into_iter()
    .collect();
    (candidate, policy, routes)
}

fn lookup(routes: &Routes) -> impl Fn(&str) -> Result<Value, CiError> + '_ {
    |path| {
        routes
            .get(path)
            .cloned()
            .ok_or_else(|| CiError::Message(format!("KeyError: {path}")))
    }
}

#[test]
fn complete_observation_remains_untrusted() {
    let (candidate, policy, routes) = inputs();
    let result = fetch_github_ci_evidence(&candidate, &policy, 45, &lookup(&routes)).unwrap();
    assert_eq!(
        result.run.run_attempt,
        routes[&format!("{ROOT}/actions/runs/45")]["run_attempt"]
            .as_i64()
            .unwrap()
    );
    assert_eq!(
        evaluate_ci_observation(&candidate, &result, &policy).unwrap(),
        CIEvaluation::PassingObservation
    );
}

#[test]
fn run_mismatch_fails_closed() {
    for (field, value) in [
        ("head_sha", json!("0".repeat(40))),
        ("run_attempt", json!(0)),
        ("id", json!(46)),
    ] {
        let (candidate, policy, mut routes) = inputs();
        routes.get_mut(&format!("{ROOT}/actions/runs/45")).unwrap()[field] = value;
        assert!(
            fetch_github_ci_evidence(&candidate, &policy, 45, &lookup(&routes)).is_err(),
            "{field}"
        );
    }
}

#[test]
fn tree_mismatch_fails_closed() {
    let (candidate, policy, mut routes) = inputs();
    routes
        .get_mut(&format!("{ROOT}/git/commits/{}", commit()))
        .unwrap()["tree"] = json!({"sha": "0".repeat(40)});
    let err = fetch_github_ci_evidence(&candidate, &policy, 45, &lookup(&routes)).unwrap_err();
    assert!(err.to_string().contains("tree"), "{err}");
}

#[test]
fn incomplete_pagination_fails_closed() {
    let (candidate, policy, mut routes) = inputs();
    routes
        .get_mut(&format!(
            "{ROOT}/actions/runs/45/attempts/2/jobs?per_page=100&page=1"
        ))
        .unwrap()["total_count"] = json!(2);
    routes.insert(
        format!("{ROOT}/actions/runs/45/attempts/2/jobs?per_page=100&page=2"),
        json!({"total_count": 2, "jobs": []}),
    );
    let err = fetch_github_ci_evidence(&candidate, &policy, 45, &lookup(&routes)).unwrap_err();
    assert!(err.to_string().contains("pagination"), "{err}");
}

#[test]
fn duplicate_job_ids_fails_closed() {
    let (candidate, policy, mut routes) = inputs();
    let key = format!("{ROOT}/actions/runs/45/attempts/2/jobs?per_page=100&page=1");
    let page = routes.get_mut(&key).unwrap();
    let jobs = page["jobs"].as_array().unwrap().clone();
    page["jobs"] = Value::Array([jobs.clone(), jobs].concat());
    page["total_count"] = json!(2);
    let err = fetch_github_ci_evidence(&candidate, &policy, 45, &lookup(&routes)).unwrap_err();
    assert!(err.to_string().contains("Duplicate"), "{err}");
}

#[test]
fn invalid_repository_blocks_reader() {
    let (candidate, mut policy, routes) = inputs();
    policy.repository = "../x".to_owned();
    let err = fetch_github_ci_evidence(&candidate, &policy, 45, &lookup(&routes)).unwrap_err();
    assert!(err.to_string().contains("repository"), "{err}");
}

#[test]
fn no_token_injection_or_live_fetch() {
    let (candidate, policy, routes) = inputs();
    let paths: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let reader = |path: &str| {
        paths.borrow_mut().push(path.to_owned());
        routes
            .get(path)
            .cloned()
            .ok_or_else(|| CiError::Message("KeyError".to_owned()))
    };
    fetch_github_ci_evidence(&candidate, &policy, 45, &reader).unwrap();
    let paths = paths.borrow();
    assert_eq!(paths.len(), routes.len());
    assert!(paths.iter().all(|p| p.starts_with("/repos/owner/repo/")));
}
