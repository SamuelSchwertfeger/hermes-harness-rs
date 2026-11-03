#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Regression tests for the Hermes skill bridge over the canonical runtime.

use std::path::{Path, PathBuf};
use std::process::Command;

use hermes_harness::native::workflows::{
    assemble, game_plan_file, inspect_run, investigate, parse_plan,
};
use serde_json::{Value, json};

fn repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir(&repo).unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "baseline",
            ])
            .status()
            .unwrap()
            .success()
    );
    repo
}

fn plan(tmp: &Path) -> PathBuf {
    let path = tmp.join("plan.json");
    let document = json!({
        "id": "offline-01",
        "objective": "offline evidence",
        "nodes": [
            {"id": "a", "objective": "create evidence", "owned_paths": ["evidence.txt"]},
            {
                "id": "b",
                "objective": "create more",
                "owned_paths": ["more.txt"],
                "dependencies": ["a"],
            },
        ],
    });
    std::fs::write(&path, document.to_string()).unwrap();
    path
}

#[test]
fn investigate_reads_repo_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let result = investigate(&repo(tmp.path())).unwrap();
    assert_eq!(result["mode"], "read-only");
    assert_eq!(result["tracked_count"], 0);
    assert_eq!(result["head"].as_str().unwrap().len(), 40);
}

#[test]
fn investigate_rejects_not_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let error = investigate(tmp.path()).unwrap_err();
    assert!(error.to_string().contains("Git checkout"), "{error}");
}

#[test]
fn game_plan_uses_real_dag() {
    let tmp = tempfile::tempdir().unwrap();
    let result = game_plan_file(&plan(tmp.path())).unwrap();
    assert_eq!(result["execution_order"], json!(["a", "b"]));
}

#[test]
fn invalid_node_shape_is_rejected() {
    let error = parse_plan(&json!({
        "id": "ok",
        "objective": "ok",
        "nodes": [{"id": "n", "objective": "ok", "owned_paths": "../bad"}],
    }))
    .unwrap_err();
    assert!(error.to_string().contains("Invalid node fields"), "{error}");
}

#[test]
fn overlapping_paths_rejected() {
    let error = parse_plan(&json!({
        "id": "ok",
        "objective": "ok",
        "nodes": [
            {"id": "a", "objective": "ok", "owned_paths": ["foo"]},
            {"id": "b", "objective": "ok", "owned_paths": ["foo/bar"]},
        ],
    }))
    .unwrap_err();
    assert!(
        error.to_string().contains("Overlapping ownership"),
        "{error}"
    );
}

#[test]
fn offline_dispatch_and_inspection() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let state = tmp.path().join("state");
    let result = assemble(&repo, &state, &plan(tmp.path())).unwrap();
    assert_eq!(result["mode"], "offline-simulated");
    assert_eq!(result["result"]["state"], "pending_human_decision");
    let states: Vec<&Value> = result["result"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| &row["state"])
        .collect();
    assert_eq!(states, ["completed", "completed"]);
    let inspected = inspect_run(&state, "offline-01").unwrap();
    assert_eq!(inspected["result"]["state"], "pending_human_decision");
    let again = assemble(&repo, &state, &plan(tmp.path())).unwrap_err();
    assert!(again.is_already_exists(), "{again}");
}

#[cfg(unix)]
#[test]
fn plan_symlink_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let file = plan(tmp.path());
    let link = tmp.path().join("link.json");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    let error = game_plan_file(&link).unwrap_err();
    assert!(error.to_string().contains("Plan must be a real"), "{error}");
}

#[test]
fn skills_exist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("hermes_skills");
    for name in ["lets-investigate", "game-plan", "agents-assemble"] {
        let contents = std::fs::read_to_string(root.join(name).join("SKILL.md")).unwrap();
        assert!(contents.contains(&format!("name: {name}")));
        assert!(contents.contains("target/release/hermes-"));
    }
}
