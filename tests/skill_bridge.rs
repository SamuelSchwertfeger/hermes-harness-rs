#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Hermes terminal bridge contract tests, without network or live execution.

use std::cell::RefCell;
use std::path::Path;

use clap::Parser;
use hermes_harness::native::skill_bridge::{Args, BridgeCommand, handle, handle_with};
use hermes_harness::native::skill_install::SKILLS;
use serde_json::json;

#[test]
fn borrowed_render_preserves_sorted_fields_overrides_and_scalar_wrapping() {
    use hermes_harness::native::skill_bridge::render;
    use hermes_harness::util::sorted_json_spaced;

    let cases = [
        json!({"z": [1.0, -0.0, 1e-7], "a": {"z": "é\n😀", "a": []}}),
        json!({"z": null, "ok": false, "a": {}}),
        json!({"ok": {"z": 2, "a": 1}}),
        json!({}),
        json!(["é", {"b": 2, "a": 1}]),
        json!("text\n"),
        json!(null),
    ];
    for output in cases {
        let mut expected = serde_json::Map::new();
        expected.insert("ok".to_owned(), json!(true));
        if let serde_json::Value::Object(fields) = &output {
            expected.extend(fields.clone());
        } else {
            expected.insert("result".to_owned(), output.clone());
        }
        assert_eq!(
            render(&output).unwrap(),
            sorted_json_spaced(&serde_json::Value::Object(expected)).unwrap()
        );
    }
}

fn parse(argv: &[&str]) -> Args {
    Args::try_parse_from(std::iter::once("bridge").chain(argv.iter().copied())).unwrap()
}

fn error_text(argv: &[&str]) -> String {
    handle(&parse(argv)).unwrap_err().to_string()
}

#[test]
fn doctor_cannot_claim_live_dispatch() {
    let output = handle(&parse(&["doctor"])).unwrap();
    assert_eq!(output["live_execution_qualified"], json!(false));
    assert_eq!(output["tool_sandbox_qualified"], json!(false));
}

#[test]
fn plan_bridge_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plan.json");
    std::fs::write(
        &path,
        json!({
            "id": "task",
            "objective": "Tests",
            "nodes": [{"id": "one", "objective": "Write tests", "owned_paths": ["tests/a.py"]}],
        })
        .to_string(),
    )
    .unwrap();
    let output = handle(&parse(&[
        "plan",
        "--file",
        path.to_str().unwrap(),
        "--session",
        "sess",
    ]))
    .unwrap();
    assert_eq!(output["ordered_nodes"], json!(["one"]));
    assert_eq!(output["dispatched"], json!(false));
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries, vec![path]);
}

#[cfg(unix)]
#[test]
fn plan_bridge_rejects_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::write(&source, "{}").unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    let text = error_text(&[
        "plan",
        "--file",
        link.to_str().unwrap(),
        "--session",
        "sess",
    ]);
    assert!(text.contains("symlinked"), "{text}");
}

#[test]
fn rejects_empty_session() {
    let dir = tempfile::tempdir().unwrap();
    let text = error_text(&[
        "inspect",
        "--state",
        dir.path().to_str().unwrap(),
        "--run-id",
        "x",
        "--session",
        " ",
    ]);
    assert!(text.contains("session"), "{text}");
}

#[test]
fn apply_parser_does_not_implicitly_authorize() {
    let args = parse(&[
        "apply-reviewed",
        "--repo",
        "/tmp/source",
        "--receipt",
        "/tmp/receipt.json",
        "--workspace",
        "/tmp/worktree",
        "--session",
        "session-1",
    ]);
    let BridgeCommand::ApplyReviewed {
        authorize_apply, ..
    } = args.command
    else {
        panic!("wrong command");
    };
    assert!(!authorize_apply);
}

fn apply_args(dir: &Path, grant: bool) -> Args {
    let repo = dir.to_str().unwrap();
    let receipt = dir.join("receipt.json");
    let workspace = dir.join("worktree");
    let mut argv = vec![
        "apply-reviewed",
        "--repo",
        repo,
        "--receipt",
        receipt.to_str().unwrap(),
        "--workspace",
        workspace.to_str().unwrap(),
        "--session",
        "session-1",
    ];
    if grant {
        argv.push("--authorize-apply");
    }
    parse(&argv)
}

#[test]
fn apply_bridge_denies_without_grant() {
    let dir = tempfile::tempdir().unwrap();
    let text = handle(&apply_args(dir.path(), false))
        .unwrap_err()
        .to_string();
    assert!(text.contains("authorization"), "{text}");
}

#[test]
fn apply_bridge_delegates_with_explicit_grant() {
    let dir = tempfile::tempdir().unwrap();
    let args = apply_args(dir.path(), true);
    let calls = RefCell::new(Vec::new());
    let response = handle_with(&args, &|repo, receipt, workspace, authorized| {
        calls.borrow_mut().push((
            repo.to_owned(),
            receipt.to_owned(),
            workspace.to_owned(),
            authorized,
        ));
        Ok(json!({"mode": "authorized-disposable-worktree", "task_id": "test-1"}))
    })
    .unwrap();
    let calls = calls.borrow();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].3);
    assert_eq!(calls[0].0, dir.path());
    assert_eq!(response["session_id"], json!("session-1"));
    assert_eq!(response["task_id"], json!("test-1"));
}

#[test]
fn skill_manifest_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("hermes_skills");
    for skill in SKILLS {
        let content = std::fs::read_to_string(root.join(skill).join("SKILL.md"))
            .unwrap()
            .replace("\r\n", "\n");
        let rest = content.strip_prefix("---\n").expect(skill);
        let (frontmatter, body) = rest.split_once("\n---\n").unwrap();
        assert!(
            frontmatter
                .lines()
                .any(|line| line == format!("name: {skill}")),
            "{skill}"
        );
        let descriptions: Vec<&str> = frontmatter
            .lines()
            .filter_map(|line| line.strip_prefix("description: "))
            .collect();
        assert_eq!(descriptions.len(), 1, "{skill}");
        let length = descriptions[0].chars().count();
        assert!(length > 0 && length <= 60, "{skill}");
        assert!(descriptions[0].ends_with('.'), "{skill}");
        assert!(frontmatter.contains("platforms: [linux]"), "{skill}");
        assert!(frontmatter.contains("author: "), "{skill}");
        assert!(frontmatter.contains("license: "), "{skill}");
        assert!(frontmatter.contains("metadata:"), "{skill}");
        assert!(body.contains("## When to Use"), "{skill}");
        assert!(body.contains("## Pitfalls"), "{skill}");
        assert!(body.contains("## Verification"), "{skill}");
        assert!(content.len() <= 100_000, "{skill}");
    }
}
