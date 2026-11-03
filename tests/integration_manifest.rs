#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Integration candidate preflight is read-only and fail-closed.

use std::path::Path;
use std::process::Command;

use hermes_harness::multiagent::integration_manifest::{
    IntegrationCandidate, IntegrationError, materialize_integration, prepare_integration,
};
use hermes_harness::multiagent::model_edits::parse_edit_proposal;
use hermes_harness::multiagent::model_pipeline::ProposalReviewResult;
use hermes_harness::multiagent::model_review::{ModelReview, Verdict};
use serde_json::json;

fn baseline() -> String {
    "a".repeat(40)
}

fn candidate(node: &str, path: &str, baseline: &str, verdict: Verdict) -> IntegrationCandidate {
    let body = json!({"task_id": node, "files": [{"path": path, "content": node}]}).to_string();
    let proposal = parse_edit_proposal(&body, node, &["src".to_owned()]).unwrap();
    let review = ModelReview::new(node, "reviewer", verdict, vec![]);
    IntegrationCandidate::new(node, baseline, ProposalReviewResult::new(proposal, review))
}

fn accepted(node: &str, path: &str) -> IntegrationCandidate {
    candidate(node, path, &baseline(), Verdict::Accept)
}

#[test]
fn combines_disjoint_reviewed_candidates() {
    let result =
        prepare_integration(&[accepted("one", "src/a.py"), accepted("two", "src/b.py")]).unwrap();
    assert_eq!(result.nodes, ["one", "two"]);
    assert_eq!(result.files, ["src/a.py", "src/b.py"]);
    assert!(!result.integrated);
}

#[test]
fn rejects_divergent_baseline() {
    let error = prepare_integration(&[
        accepted("one", "src/a.py"),
        candidate("two", "src/b.py", &"b".repeat(40), Verdict::Accept),
    ])
    .unwrap_err();
    assert!(error.to_string().contains("Divergent"));
}

#[test]
fn rejects_conflicting_paths() {
    let error = prepare_integration(&[accepted("one", "src/a.py"), accepted("two", "src/a.py")])
        .unwrap_err();
    assert!(error.to_string().contains("Overlapping"));
}

#[test]
fn rejects_unapproved_review() {
    let error = prepare_integration(&[candidate(
        "one",
        "src/a.py",
        &baseline(),
        Verdict::ChangesRequired,
    )])
    .unwrap_err();
    assert!(error.to_string().contains("accepting"));
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn materializes_multiple_disjoint_files_in_integration_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "baseline",
        ],
    );
    let baseline = git(&repo, &["rev-parse", "HEAD"]);
    let worktree = tmp.path().join("integration");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "harness/integration",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let candidates = [
        candidate("one", "src/a.py", &baseline, Verdict::Accept),
        candidate("two", "src/b.py", &baseline, Verdict::Accept),
    ];
    let error = materialize_integration(&worktree, &candidates, false).unwrap_err();
    assert!(matches!(error, IntegrationError::Workflow(_)));
    assert!(error.to_string().contains("authorization"));
    let result = materialize_integration(&worktree, &candidates, true).unwrap();
    assert_eq!(result.audit.changed_paths, ["src/a.py", "src/b.py"]);
    assert_eq!(
        std::fs::read_to_string(worktree.join("src/a.py")).unwrap(),
        "one"
    );
    assert!(!repo.join("src/a.py").exists());
}
