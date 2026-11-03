#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Real-process transport tests; no model calls or network traffic.
//!
//! The unix tests call `ForegroundFixtureWorker::run` directly rather than going through
//! `supervised::orchestration::dispatch_offline`.

use hermes_harness::core::transport::{ForegroundOptions, TransportError};

#[test]
fn reject_live_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("hermes");
    std::fs::write(&path, "fake").unwrap();
    let options = ForegroundOptions::new(vec![path.to_string_lossy().into_owned()], tmp.path());
    let err = options.validate().unwrap_err();
    assert!(matches!(err, TransportError::Permission(_)));
    assert!(err.to_string().contains("Live model"));
}

#[cfg(unix)]
mod process {
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use hermes_harness::core::transport::{
        ForegroundFixtureWorker, ForegroundOptions, TransportError,
    };
    use hermes_harness::core::watchdog::Budget;
    use hermes_harness::gateway::hermes_adapter::PreparedInvocation;
    use serde_json::Value;

    fn invocation(workspace: &Path) -> PreparedInvocation {
        PreparedInvocation {
            ticket_id: "fixture-001".to_owned(),
            kind: "scout".to_owned(),
            provider: "ollama".to_owned(),
            model: "granite4:3b".to_owned(),
            max_turns: 2,
            deadline_seconds: 60,
            workspace: workspace.to_string_lossy().into_owned(),
            prompt: "Summarize offline fixture".to_owned(),
            execution_enabled: false,
            disabled_reason: "fixture".to_owned(),
        }
    }

    fn options(tmp: &Path, code: &str, deadline: f64) -> ForegroundOptions {
        let mut options = ForegroundOptions::new(
            vec!["/bin/sh".to_owned(), "-c".to_owned(), code.to_owned()],
            tmp.join("transport"),
        );
        options.budget = Budget {
            deadline_seconds: deadline,
            grace_seconds: 0.2,
            poll_seconds: 0.05,
        };
        options
    }

    fn glob_one(dir: &Path, depth: usize, name: &str) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        let mut level = vec![dir.to_path_buf()];
        for _ in 0..depth {
            level = level
                .iter()
                .filter(|d| d.is_dir())
                .flat_map(|d| std::fs::read_dir(d).unwrap())
                .map(|e| e.unwrap().path())
                .collect();
        }
        for entry in level {
            if entry.file_name().unwrap() == name {
                found.push(entry);
            }
        }
        found
    }

    #[test]
    fn request_handoff_and_audit() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let worker =
            ForegroundFixtureWorker::new(options(tmp.path(), "cat \"$0\"", 3.0), None).unwrap();
        let output = worker.run(&invocation(tmp.path())).unwrap();
        let echoed: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(echoed["kind"], "scout");
        let transport = tmp.path().join("transport");
        let requests = glob_one(&transport, 2, "request.json");
        assert_eq!(requests.len(), 1);
        let mode = std::fs::metadata(&requests[0])
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(&requests[0]).unwrap()).unwrap();
        assert_eq!(request["execution_enabled"], false);
        let records = glob_one(&transport, 3, "run.json");
        assert_eq!(records.len(), 1);
        let record: Value =
            serde_json::from_str(&std::fs::read_to_string(&records[0]).unwrap()).unwrap();
        assert_eq!(record["outcome"], "completed");
    }

    #[test]
    fn nonzero_failure_is_not_success() {
        let tmp = tempfile::tempdir().unwrap();
        let worker =
            ForegroundFixtureWorker::new(options(tmp.path(), "exit 3", 3.0), None).unwrap();
        let err = worker.run(&invocation(tmp.path())).unwrap_err();
        assert!(matches!(err, TransportError::Transport(_)));
        assert!(err.to_string().contains("without success"));
    }

    #[test]
    fn timeout_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let worker =
            ForegroundFixtureWorker::new(options(tmp.path(), "sleep 20", 0.1), None).unwrap();
        let err = worker.run(&invocation(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("timed_out"));
    }

    #[test]
    fn cancelled_before_dispatch() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        let worker =
            ForegroundFixtureWorker::new(options(tmp.path(), "sleep 20", 3.0), Some(&cancel))
                .unwrap();
        let err = worker.run(&invocation(tmp.path())).unwrap_err();
        assert!(err.to_string().contains("cancelled"));
    }
}
