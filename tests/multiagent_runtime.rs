#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Integrated deterministic multi-worktree flow and fail-closed validation.

use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::multiagent::runtime::{
    Decision, DemoFactory, ExecutionConfig, Node, Plan, RunState, SharedAgent, TaskState,
    WorkerFactory, WorkflowError, decide, execute, inspect, ordered_nodes,
};
use serde_json::json;

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success());
}

fn repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "dev"]);
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
            "initial",
        ],
    );
    repo
}

fn plan() -> Plan {
    Plan::new(
        "run",
        "Two edits",
        vec![
            Node::new("b", "second", &["b.txt"]).with_dependencies(&["a"]),
            Node::new("a", "first", &["a.txt"]),
        ],
    )
}

#[test]
fn full_temporary_git_repo_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let state = tmp.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let ids: Vec<String> = ordered_nodes(&plan())
        .unwrap()
        .into_iter()
        .map(|n| n.id)
        .collect();
    assert_eq!(ids, ["a", "b"]);
    let factory = DemoFactory;
    let config = ExecutionConfig::new(&repo, &factory);
    let result = execute(&state, &plan(), &config).unwrap();
    assert_eq!(result.state, RunState::PendingHumanDecision);
    assert_eq!(result.tasks.len(), 2);
    assert!(
        result
            .tasks
            .iter()
            .all(|node| node.state == TaskState::Completed)
    );
    assert_eq!(
        std::fs::read_to_string(state.join("run/workspaces/a/a.txt")).unwrap(),
        "offline evidence for a\n"
    );
    assert_eq!(
        std::fs::read_to_string(state.join("run/workspaces/b/b.txt")).unwrap(),
        "offline evidence for b\n"
    );
    assert!(!repo.join("a.txt").exists());
    assert_eq!(
        inspect(&state, "run").unwrap().decision,
        Some(json!({"state": "pending", "approved": false, "integrated": false, "run_id": "run"}))
    );
    assert!(!decide(&state, "run", Decision::Reject).unwrap().applied);
    assert!(
        decide(&state, "run", Decision::Approve)
            .unwrap_err()
            .is_already_exists()
    );
    assert!(
        execute(&state, &plan(), &config)
            .unwrap_err()
            .is_already_exists()
    );
}

#[test]
fn rejects_unsafe_plans() {
    let cases: Vec<Vec<Node>> = vec![
        vec![
            Node::new("a", "test", &["a"]).with_dependencies(&["b"]),
            Node::new("b", "test", &["b"]).with_dependencies(&["a"]),
        ],
        vec![
            Node::new("a", "test", &["x"]),
            Node::new("b", "test", &["x/y"]),
        ],
        vec![Node::new("a", "test", &["../escape"])],
        vec![Node::new("a", "test", &[".git/config"])],
        vec![Node::new("a", "test", &["a"]).with_dependencies(&["missing"])],
    ];
    for nodes in cases {
        assert!(ordered_nodes(&Plan::new("run", "test", nodes)).is_err());
    }
}

struct FailingFactory;

impl WorkerFactory for FailingFactory {
    fn worker(&self, _node: &Node, _workspace: &Path) -> Result<SharedAgent, WorkflowError> {
        Err(WorkflowError::msg("simulated failure before dispatch"))
    }

    fn reviewer(&self, node: &Node) -> Result<SharedAgent, WorkflowError> {
        DemoFactory.reviewer(node)
    }
}

#[test]
fn failure_is_fenced_without_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let state = tmp.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let failing = FailingFactory;
    assert!(execute(&state, &plan(), &ExecutionConfig::new(&repo, &failing)).is_err());
    assert_eq!(
        inspect(&state, "run").unwrap().state,
        RunState::FencedOrIncomplete
    );
    let demo = DemoFactory;
    let error = execute(&state, &plan(), &ExecutionConfig::new(&repo, &demo)).unwrap_err();
    assert!(error.is_already_exists());
}
