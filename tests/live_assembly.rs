#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
use hermes_harness::native::live_assembly::{
    LiveAssembly, apply_live_assembly, live_assemble, live_assemble_with,
};
use serde_json::json;
use std::{fs, path::Path, process::Command};
fn request<'a>(root: &'a Path) -> LiveAssembly<'a> {
    LiveAssembly {
        repo: root,
        plan: root,
        state: root,
        registry: root,
        session: "local-qualification",
        authorize_inference: false,
        authorize_tools: false,
        xps_worker_model: None,
        xps_reviewer_model: None,
    }
}
fn git(root: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    );
}
fn repo(root: &Path) {
    fs::create_dir(root).unwrap();
    git(root, &["init", "-q"]);
    fs::write(root.join("README.md"), "fixture").unwrap();
    git(root, &["add", "README.md"]);
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "initial",
        ],
    );
}
#[test]
fn live_requires_inference_grant() {
    let t = tempfile::tempdir().unwrap();
    assert!(
        live_assemble(&request(t.path()))
            .unwrap_err()
            .to_string()
            .contains("inference grant")
    );
}
#[test]
fn live_never_creates_repo_or_state_before_authorization() {
    let t = tempfile::tempdir().unwrap();
    let target = t.path().join("missing");
    assert!(
        live_assemble(&request(&target))
            .unwrap_err()
            .to_string()
            .contains("inference grant")
    );
    assert!(!target.exists());
}
#[test]
fn apply_requires_independent_grant() {
    let t = tempfile::tempdir().unwrap();
    let w = t.path().join("new-worktree");
    assert!(
        apply_live_assembly(t.path(), &t.path().join("receipt.json"), &w, false)
            .unwrap_err()
            .to_string()
            .contains("application grant")
    );
    assert!(!w.exists());
}
#[test]
fn apply_rejects_untrusted_receipt_before_worktree() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().join("repo");
    repo(&r);
    let receipt = t.path().join("receipt.json");
    fs::write(&receipt, r#"{"mode":"untrusted","verdict":"accept"}"#).unwrap();
    let w = t.path().join("new-worktree");
    assert!(
        apply_live_assembly(&r, &receipt, &w, true)
            .unwrap_err()
            .to_string()
            .contains("not eligible")
    );
    assert!(!w.exists());
}
fn invoke_xps(root: &Path, reviewer: Option<&str>) -> String {
    let r = root.join("repo");
    repo(&r);
    let plan = root.join("plan.json");
    fs::write(&plan,serde_json::to_vec(&json!({"id":"plan","objective":"test","nodes":[{"id":"coding1","objective":"test","owned_paths":["README.md"]}]})).unwrap()).unwrap();
    let mut req = request(&r);
    req.plan = &plan;
    req.state = root;
    req.authorize_inference = true;
    req.xps_worker_model = Some("granite4:3b");
    req.xps_reviewer_model = reviewer;
    live_assemble_with(&req, || Ok(vec!["granite4:3b".to_owned()]), None)
        .unwrap_err()
        .to_string()
}
#[test]
fn xps_requires_worker_and_reviewer_pair() {
    let t = tempfile::tempdir().unwrap();
    assert!(invoke_xps(t.path(), None).contains("requires both worker"));
    assert!(!t.path().join("coding1-live-assembly-claim.json").exists());
}
#[test]
fn xps_rejects_unknown_model_without_claim() {
    let t = tempfile::tempdir().unwrap();
    assert!(invoke_xps(t.path(), Some("missing:7b")).contains("not currently installed"));
    assert!(!t.path().join("coding1-live-assembly-claim.json").exists());
}
