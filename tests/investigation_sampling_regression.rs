#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;
use std::process::Command;

use hermes_harness::native::workflows::investigate;
use serde_json::json;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn investigation_counts_all_lines_and_keeps_ordered_samples_at_limit() {
    for count in [0, 40, 41] {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["commit", "--allow-empty", "-qm", "baseline"]);
        for index in (0..count).rev() {
            std::fs::write(
                repo.path().join(format!("file-{index:02}.txt")),
                b"sample\n",
            )
            .unwrap();
        }
        git(repo.path(), &["add", "--", "."]);

        let report = investigate(repo.path()).unwrap();
        let paths: Vec<String> = (0..count.min(40))
            .map(|index| format!("file-{index:02}.txt"))
            .collect();
        let changes: Vec<String> = paths.iter().map(|path| format!("A  {path}")).collect();
        assert_eq!(report["tracked_count"], json!(count));
        assert_eq!(report["tracked_sample"], json!(paths));
        assert_eq!(report["tracked_truncated"], json!(count > 40));
        assert_eq!(report["changes"], json!(changes));
        assert_eq!(report["changes_truncated"], json!(count > 40));
    }
}
