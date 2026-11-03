//! Process-group watchdog for owned, foreground child commands only.
//!
//! Never assumes terminating a Hermes CLI terminates gateway-accepted work.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Default timeout in seconds, matching the Python signature.
pub const DEFAULT_TIMEOUT_SECS: f64 = 30.0;

const POLL: Duration = Duration::from_millis(5);
const TERM_GRACE: Duration = Duration::from_secs(2);

/// Errors raised before or while launching a bounded command.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    /// Python `ValueError`: empty command or non-positive timeout.
    #[error("{0}")]
    Value(&'static str),
    /// The child could not be spawned or waited on.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Outcome of a bounded run (Python `Result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    /// Exit code; negative signal number when killed by a signal.
    pub returncode: i32,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// True when the watchdog had to terminate the process group.
    pub timed_out: bool,
}

#[cfg(unix)]
fn returncode(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .unwrap_or(-1)
}

#[cfg(not(unix))]
fn returncode(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

/// Decode like Python's `text=True` (UTF-8, universal newlines).
fn decode(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn drain<R: std::io::Read + Send + 'static>(stream: Option<R>) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut buf);
        }
        buf
    })
}

/// Build a command that runs in a new session (Python `start_new_session=True`).
///
/// On unix this launches through util-linux `setsid`; the child is not a group leader, so
/// `setsid` execs in place and the child pid equals the new session and group id. `unsafe` is
/// forbidden, so `pre_exec(setsid)` is impossible; when no `setsid` binary exists this falls
/// back to `process_group(0)` so the child still leads its own process group (the group is what
/// `signal_group` targets), though it stays in the parent's session.
pub(crate) fn session_command(argv: &[String]) -> Option<Command> {
    let (program, rest) = argv.split_first()?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let probe = ["/usr/bin/setsid", "/bin/setsid"]
            .into_iter()
            .find(|path| Path::new(path).is_file());
        if let Some(setsid) = probe {
            let mut command = Command::new(setsid);
            command.arg("--").arg(program).args(rest);
            return Some(command);
        }
        let mut command = Command::new(program);
        command.args(rest).process_group(0);
        Some(command)
    }
    #[cfg(not(unix))]
    {
        let mut command = Command::new(program);
        command.args(rest);
        Some(command)
    }
}

/// Wait until the child leads its own process group.
///
/// Through the `setsid` binary the group only exists once `setsid` has run, a moment after
/// `spawn` returns; Python's `start_new_session` has it ready at spawn time. Without this wait a
/// prompt `killpg` would hit `ESRCH` and miss the child. Bounded; a no-op off unix.
#[cfg(unix)]
pub(crate) fn await_group(child: &Child) {
    use rustix::process::{Pid, test_kill_process_group};
    let Some(group) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) else {
        return;
    };
    let end = Instant::now() + Duration::from_secs(1);
    while test_kill_process_group(group).is_err() && Instant::now() < end {
        thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(not(unix))]
pub(crate) const fn await_group(_child: &Child) {}

fn build(argv: &[String], cwd: &Path) -> Option<Command> {
    let mut command = session_command(argv)?;
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .env("HOME", cwd);
    #[cfg(windows)]
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    Some(command)
}

/// Signal sent to a process group.
#[derive(Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

/// Signal the whole process group of `child` (SIGTERM or SIGKILL), like `os.killpg`.
///
/// Returns false when the signal could not be sent.
#[cfg(unix)]
#[allow(clippy::needless_pass_by_ref_mut)] // shares a signature with the non-unix variant
fn signal_group(child: &mut Child, signal: Signal) -> bool {
    use rustix::process::{Pid, Signal as Sig, kill_process_group};
    i32::try_from(child.id())
        .ok()
        .and_then(Pid::from_raw)
        .is_some_and(|pgid| {
            let signal = match signal {
                Signal::Term => Sig::Term,
                Signal::Kill => Sig::Kill,
            };
            kill_process_group(pgid, signal).is_ok()
        })
}

#[cfg(not(unix))]
fn signal_group(child: &mut Child, _signal: Signal) -> bool {
    let _ = child.kill();
    true
}

fn wait_until(child: &mut Child, limit: Option<Duration>) -> std::io::Result<bool> {
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        if limit.is_some_and(|max| start.elapsed() >= max) {
            return Ok(false);
        }
        thread::sleep(POLL);
    }
}

/// Run `argv` in `cwd` with a minimal environment, terminating its process group on timeout.
///
/// # Errors
/// The command cannot be started, its output cannot be read, or supervision fails.
pub fn run_bounded(
    argv: &[String],
    cwd: &Path,
    timeout_secs: f64,
) -> Result<RunResult, ExecutionError> {
    let invalid = ExecutionError::Value("Expected a command and positive timeout");
    if !timeout_secs.is_finite() || timeout_secs <= 0.0 {
        return Err(invalid);
    }
    let limit = Duration::try_from_secs_f64(timeout_secs)
        .map_err(|_| ExecutionError::Value("Expected a command and positive timeout"))?;
    let Some(mut command) = build(argv, cwd) else {
        return Err(invalid);
    };
    let mut child = command.spawn()?;
    await_group(&child);
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let mut timed_out = false;
    if !wait_until(&mut child, Some(limit))? {
        timed_out = true;
        // If TERM cannot be sent, still honour the grace period before the hard kill.
        let _ = signal_group(&mut child, Signal::Term);
        if !wait_until(&mut child, Some(TERM_GRACE))? {
            let _ = signal_group(&mut child, Signal::Kill);
            let _ = child.kill();
        }
    }
    let status = child.wait()?;
    Ok(RunResult {
        returncode: returncode(status),
        stdout: decode(&out.join().unwrap_or_default()),
        stderr: decode(&err.join().unwrap_or_default()),
        timed_out,
    })
}
