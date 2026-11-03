#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]

use hermes_harness::core::execution::{DEFAULT_TIMEOUT_SECS, run_bounded};

#[cfg(unix)]
fn say_ok() -> Vec<String> {
    vec!["sh".into(), "-c".into(), "echo ok".into()]
}

#[cfg(not(unix))]
fn say_ok() -> Vec<String> {
    vec!["cmd".into(), "/C".into(), "echo ok".into()]
}

#[cfg(unix)]
fn sleep_long() -> Vec<String> {
    vec!["sh".into(), "-c".into(), "sleep 30".into()]
}

#[cfg(not(unix))]
fn sleep_long() -> Vec<String> {
    vec!["ping".into(), "-n".into(), "30".into(), "127.0.0.1".into()]
}

#[test]
fn success() {
    let dir = tempfile::tempdir().unwrap();
    let result = run_bounded(&say_ok(), dir.path(), DEFAULT_TIMEOUT_SECS).unwrap();
    assert_eq!(result.returncode, 0);
    assert_eq!(result.stdout.trim(), "ok");
}

#[test]
fn timeout() {
    let dir = tempfile::tempdir().unwrap();
    let result = run_bounded(&sleep_long(), dir.path(), 0.1).unwrap();
    assert!(result.timed_out);
}

#[cfg(target_os = "linux")]
#[test]
fn child_runs_in_new_session() {
    fn session_of(stat: &str) -> String {
        let after = stat.rsplit_once(')').unwrap().1;
        after.split_whitespace().nth(3).unwrap().to_owned()
    }
    let own = session_of(&std::fs::read_to_string("/proc/self/stat").unwrap());
    let argv = vec!["cat".to_owned(), "/proc/self/stat".to_owned()];
    let out = run_bounded(&argv, &std::env::temp_dir(), 10.0).unwrap();
    assert_eq!(out.returncode, 0);
    assert_ne!(session_of(&out.stdout), own);
}

#[test]
fn rejects_nonfinite_and_nonpositive_timeouts_before_spawn() {
    use hermes_harness::core::execution::ExecutionError;
    let dir = tempfile::tempdir().unwrap();
    let command = vec!["nonexistent-timeout-test-executable".to_owned()];
    for timeout in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        0.0,
        -1.0,
        f64::MAX,
    ] {
        assert!(matches!(
            run_bounded(&command, dir.path(), timeout),
            Err(ExecutionError::Value(_))
        ));
    }
}
