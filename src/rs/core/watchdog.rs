//! Supervise owned foreground process groups; never supervise detached Hermes gateway jobs.

use crate::util::create_private_dir;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::execution::{await_group, session_command};
use crate::util::{pretty_json_ordered, utc_now_iso, uuid4_hex};

/// Errors from supervision.
#[derive(Debug, thiserror::Error)]
pub enum WatchdogError {
    /// Invalid arguments (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// Filesystem or process failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Record serialisation failure.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

/// How a supervised run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Child exited by itself.
    Completed,
    /// Deadline reached.
    TimedOut,
    /// Cancelled by the caller.
    Cancelled,
    /// The process group survived cleanup.
    CleanupFailed,
}

/// Timing budgets in seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    /// Wall-clock deadline for the child.
    pub deadline_seconds: f64,
    /// Grace per escalation step.
    pub grace_seconds: f64,
    /// Polling interval.
    pub poll_seconds: f64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            deadline_seconds: 30.0,
            grace_seconds: 1.0,
            poll_seconds: 0.05,
        }
    }
}

impl Budget {
    /// Reject non-positive budgets.
    ///
    /// # Errors
    /// Execution budgets are invalid, process supervision fails, or watchdog records cannot be
    /// written.
    pub fn validate(&self) -> Result<(), WatchdogError> {
        let positive = |v: f64| v > 0.0;
        if positive(self.deadline_seconds)
            && positive(self.grace_seconds)
            && positive(self.poll_seconds)
        {
            Ok(())
        } else {
            Err(WatchdogError::Value(
                "All watchdog timing budgets must be positive".to_owned(),
            ))
        }
    }
}

/// Where and how to run.
#[derive(Debug, Clone, PartialEq)]
pub struct SupervisionOptions {
    /// Working directory of the child.
    pub cwd: PathBuf,
    /// Directory receiving one sub-directory per run.
    pub records: PathBuf,
    /// Timing budgets.
    pub budget: Budget,
}

impl SupervisionOptions {
    /// Options with the default budget.
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>, records: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            records: records.into(),
            budget: Budget::default(),
        }
    }
}

/// Persisted result of one supervised run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Random hex run identifier.
    pub run_id: String,
    /// Command line.
    pub argv: Vec<String>,
    /// Child pid.
    pub pid: u32,
    /// Process group id (equals pid).
    pub pgid: u32,
    /// Start timestamp.
    pub started_at: String,
    /// Finish timestamp.
    pub finished_at: String,
    /// Outcome.
    pub outcome: Outcome,
    /// Exit code (negative signal number when killed), if reaped.
    pub returncode: Option<i32>,
    /// Whether the group still had members at the end.
    pub group_alive: bool,
    /// Captured stdout file.
    pub stdout_path: String,
    /// Captured stderr file.
    pub stderr_path: String,
}

fn secs(value: f64) -> Duration {
    Duration::try_from_secs_f64(value).unwrap_or_else(|_| Duration::from_secs(u64::from(u32::MAX)))
}

fn after(base: Instant, value: f64) -> Instant {
    base.checked_add(secs(value)).unwrap_or(base)
}

/// `os.killpg(pgid, 0)`: does the process group still have members?
///
/// Like Python, `ESRCH` means gone and `EPERM` means alive but not ours.
#[cfg(unix)]
#[must_use]
pub fn group_alive(pgid: u32) -> bool {
    use rustix::io::Errno;
    use rustix::process::{Pid, test_kill_process_group};
    let Some(group) = i32::try_from(pgid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    match test_kill_process_group(group) {
        Ok(()) => true,
        Err(error) => error == Errno::PERM,
    }
}

/// Process groups do not exist off unix.
#[cfg(not(unix))]
#[must_use]
pub const fn group_alive(_pgid: u32) -> bool {
    false
}

/// `os.killpg(pgid, sig)`, ignoring a missing group.
#[cfg(unix)]
pub fn signal_group(pgid: u32, signal: &str) {
    use rustix::process::{Pid, Signal, kill_process_group};
    let signal = match signal {
        "TERM" => Signal::Term,
        "KILL" => Signal::Kill,
        _ => return,
    };
    if let Some(pid) = i32::try_from(pgid).ok().and_then(Pid::from_raw) {
        let _ = kill_process_group(pid, signal);
    }
}

/// Process groups do not exist off unix.
#[cfg(not(unix))]
pub const fn signal_group(_pgid: u32, _signal: &str) {}

fn wait_until_gone(pgid: u32, budget: &Budget) {
    let end = after(Instant::now(), budget.grace_seconds);
    while Instant::now() < end && group_alive(pgid) {
        sleep(secs(budget.poll_seconds));
    }
}

fn wait_child(child: &mut Child, budget: &Budget) -> bool {
    let end = after(Instant::now(), budget.grace_seconds);
    loop {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        sleep(secs(budget.poll_seconds));
    }
}

/// Terminate only the owned process group, with bounded escalation.
pub fn quiesce(child: &mut Child, budget: &Budget) -> bool {
    let pgid = child.id();
    signal_group(pgid, "TERM");
    wait_until_gone(pgid, budget);
    if group_alive(pgid) {
        signal_group(pgid, "KILL");
    }
    wait_until_gone(pgid, budget);
    if cfg!(not(unix)) {
        let _ = child.kill();
    }
    if !wait_child(child, budget) {
        return false;
    }
    !group_alive(pgid)
}

/// Atomically write `run.json` into `directory`.
///
/// # Errors
/// Execution budgets are invalid, process supervision fails, or watchdog records cannot be written.
pub fn record(directory: &Path, result: &RunRecord) -> Result<(), WatchdogError> {
    let target = directory.join("run.json");
    let temporary = directory.join("run.json.tmp");
    fs::write(&temporary, format!("{}\n", pretty_json_ordered(result)?))?;
    fs::rename(&temporary, &target)?;
    Ok(())
}

/// Poll the child until it exits, is cancelled or times out, then clean up.
pub fn monitor(child: &mut Child, budget: &Budget, cancel: Option<&AtomicBool>) -> Outcome {
    let cutoff = after(Instant::now(), budget.deadline_seconds);
    let mut outcome = Outcome::Completed;
    while matches!(child.try_wait(), Ok(None)) {
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            outcome = Outcome::Cancelled;
            break;
        }
        if Instant::now() >= cutoff {
            outcome = Outcome::TimedOut;
            break;
        }
        sleep(secs(budget.poll_seconds));
    }
    // Off unix `group_alive` is always false, so also quiesce when the child is still running.
    let running = !matches!(child.try_wait(), Ok(Some(_)));
    if (running || group_alive(child.id())) && !quiesce(child, budget) {
        return Outcome::CleanupFailed;
    }
    if group_alive(child.id()) {
        return Outcome::CleanupFailed;
    }
    outcome
}

/// Spawn `argv` in its own process group with a minimal environment.
///
/// # Errors
/// Execution budgets are invalid, process supervision fails, or watchdog records cannot be written.
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    out_file: File,
    err_file: File,
) -> Result<Child, WatchdogError> {
    let mut command = session_command(argv).ok_or_else(|| {
        WatchdogError::Value("Expected a command and existing working directory".to_owned())
    })?;
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", cwd);
    let child = command.spawn()?;
    await_group(&child);
    Ok(child)
}

fn new_run_id() -> Result<String, WatchdogError> {
    Ok(uuid4_hex()?)
}

fn exit_code(child: &mut Child) -> Option<i32> {
    let status = child.try_wait().ok().flatten()?;
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return Some(-signal);
    }
    status.code()
}

/// Supervise a trusted foreground process group; never a detached Hermes gateway.
///
/// # Errors
/// Execution budgets are invalid, process supervision fails, or watchdog records cannot be written.
pub fn supervise(
    argv: &[String],
    options: &SupervisionOptions,
    cancel: Option<&AtomicBool>,
) -> Result<RunRecord, WatchdogError> {
    options.budget.validate()?;
    if argv.is_empty() || !options.cwd.is_dir() {
        return Err(WatchdogError::Value(
            "Expected a command and existing working directory".to_owned(),
        ));
    }
    let run_id = new_run_id()?;
    let directory = options.records.join(&run_id);
    fs::create_dir_all(&options.records)?;
    create_private_dir(&directory)?;
    let stdout_path = directory.join("stdout.log");
    let stderr_path = directory.join("stderr.log");
    let started_at = utc_now_iso();
    let mut child = spawn(
        argv,
        &options.cwd,
        File::create(&stdout_path)?,
        File::create(&stderr_path)?,
    )?;
    log::info!("watchdog start run={run_id} pid={}", child.id());
    let outcome = monitor(&mut child, &options.budget, cancel);
    let alive = group_alive(child.id());
    let result = RunRecord {
        run_id,
        argv: argv.to_vec(),
        pid: child.id(),
        pgid: child.id(),
        started_at,
        finished_at: utc_now_iso(),
        outcome,
        returncode: exit_code(&mut child),
        group_alive: alive,
        stdout_path: stdout_path.to_string_lossy().into_owned(),
        stderr_path: stderr_path.to_string_lossy().into_owned(),
    };
    record(&directory, &result)?;
    log::info!(
        "watchdog stop run={} outcome={} alive={alive}",
        result.run_id,
        match outcome {
            Outcome::Completed => "completed",
            Outcome::TimedOut => "timed_out",
            Outcome::Cancelled => "cancelled",
            Outcome::CleanupFailed => "cleanup_failed",
        }
    );
    Ok(result)
}
