//! Bounded verifier process runner for disposable worktrees.
//!
//! This executes an operator-selected command, NOT instructions from a model.
//! It is not an OS sandbox; use only with a separately restricted account or
//! container and never pass untrusted shell strings.

use crate::util::PathLexical as _;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sha2::{Digest as _, Sha256};

use crate::multiagent::sandbox_tools::which;
use crate::multiagent::verification_evidence::{
    TestEvidence, VerificationDecision, evaluate_test_evidence,
};
use crate::multiagent::verification_manifest::{VerificationError, prepare_verification};
use crate::util::is_py_blank;

/// Maximum bytes kept from each of stdout and stderr.
pub const MAX_CAPTURE: usize = 64 * 1024;
/// Upper bound for the verifier timeout in seconds.
pub const MAX_VERIFICATION_SECONDS: i64 = 180;
/// Allowlisted verifier commands: name to argv (first item is looked up on `PATH`).
pub const ALLOWED: &[(&str, &[&str])] = &[("pytest", &["uv", "run", "pytest", "-q"])];

/// Authorization flags and timeout of one verification (Python keyword arguments).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VerifyOptions {
    /// Explicit trusted-caller authorization.
    pub authorized: bool,
    /// Caller asserts the code under test is trusted.
    pub trusted_code: bool,
    /// Timeout in seconds (1 to [`MAX_VERIFICATION_SECONDS`]).
    pub timeout_seconds: i64,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            authorized: false,
            trusted_code: false,
            timeout_seconds: 60,
        }
    }
}

/// Evidence plus captured output of one verifier run.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VerificationRun {
    /// Snapshot-bound evidence.
    pub evidence: TestEvidence,
    /// Captured standard output, truncated to [`MAX_CAPTURE`] bytes.
    pub stdout: String,
    /// Captured standard error, truncated to [`MAX_CAPTURE`] bytes.
    pub stderr: String,
    /// Whether the verifier exceeded its timeout.
    pub timed_out: bool,
}

/// Background reader of one child pipe.
struct Drain {
    buffer: Arc<Mutex<Vec<u8>>>,
    handle: JoinHandle<()>,
}

impl Drain {
    fn start<R: std::io::Read + Send + 'static>(stream: Option<R>) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buffer);
        let handle = std::thread::spawn(move || {
            let Some(mut stream) = stream else { return };
            let mut chunk = [0_u8; 8192];
            while let Ok(read) = stream.read(&mut chunk) {
                let Some(data) = chunk.get(..read).filter(|data| !data.is_empty()) else {
                    return;
                };
                let mut captured = sink
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let remaining = MAX_CAPTURE.saturating_sub(captured.len());
                if let Some(head) = data.get(..data.len().min(remaining)) {
                    captured.extend_from_slice(head);
                }
            }
        });
        Self { buffer, handle }
    }

    /// Wait for EOF (`grace = None`) or at most `grace`, then return what was captured.
    fn finish(self, grace: Option<Duration>) -> Vec<u8> {
        match grace {
            None => {
                let _ = self.handle.join();
            }
            Some(grace) => {
                let end = Instant::now() + grace;
                while !self.handle.is_finished() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        std::mem::take(
            &mut *self
                .buffer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

fn exit_code_of(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status
            .code()
            .or_else(|| status.signal().map(|signal| -signal))
            .unwrap_or(-1)
    }
    #[cfg(not(unix))]
    {
        status.code().unwrap_or(-1)
    }
}

struct Outcome {
    returncode: i32,
    timed_out: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run `command` with captured output, killing it after `timeout` (Python `subprocess.run`).
fn run_captured(command: &mut Command, timeout: Duration) -> Result<Outcome, std::io::Error> {
    let mut child = command
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = Drain::start(child.stdout.take());
    let err = Drain::start(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let grace = status.is_none().then_some(Duration::from_secs(2));
    let stdout = out.finish(grace);
    let stderr = err.finish(grace);
    Ok(Outcome {
        returncode: status.map_or(124, exit_code_of),
        timed_out: status.is_none(),
        stdout,
        stderr,
    })
}

/// Execute one allowlisted verifier and bind its result to file digests.
///
/// # Errors
/// Returns [`VerificationError`] when authorization, command, identity, timeout or worktree
/// checks fail, when the snapshot changes while the verifier runs, or on I/O failure.
pub fn run_verification(
    workspace: &Path,
    baseline: &str,
    owned_paths: &[String],
    verifier_identity: &str,
    command_name: &str,
    options: VerifyOptions,
) -> Result<VerificationRun, VerificationError> {
    if !options.authorized {
        return Err(VerificationError::msg(
            "Verification requires explicit trusted-caller authorization",
        ));
    }
    if !options.trusted_code {
        return Err(VerificationError::msg(
            "Untrusted code must use a qualified isolated verifier",
        ));
    }
    let Some((_, command)) = ALLOWED.iter().find(|(name, _)| *name == command_name) else {
        return Err(VerificationError::msg("Unknown verifier command"));
    };
    if is_py_blank(verifier_identity)
        || !(1..=MAX_VERIFICATION_SECONDS).contains(&options.timeout_seconds)
    {
        return Err(VerificationError::msg(
            "Invalid verifier identity or timeout",
        ));
    }
    if workspace.is_symlink_lexical() || !workspace.is_dir() {
        return Err(VerificationError::msg(
            "Verifier worktree must be an existing real directory",
        ));
    }
    let before = prepare_verification(workspace, baseline, owned_paths)?;
    let executable = command.first().and_then(|name| which(name));
    let Some(executable) = executable else {
        return Err(VerificationError::msg("Verifier executable unavailable"));
    };
    let mut process = Command::new(executable);
    process
        .args(command.iter().skip(1))
        .current_dir(workspace)
        .env_clear()
        .env(
            "PATH",
            std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()),
        )
        .env("HOME", workspace)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONNOUSERSITE", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("UV_OFFLINE", "1")
        .env("PIP_NO_INDEX", "1")
        .env("PYTEST_DISABLE_PLUGIN_AUTOLOAD", "1");
    let seconds = u64::try_from(options.timeout_seconds).unwrap_or(1);
    let outcome = run_captured(&mut process, Duration::from_secs(seconds))?;
    let after = prepare_verification(workspace, baseline, owned_paths)?;
    if after != before {
        return Err(VerificationError::msg(
            "Verifier mutated the audited integration snapshot",
        ));
    }
    let mut hashed = Sha256::new();
    hashed.update(&outcome.stdout);
    hashed.update([0]);
    hashed.update(&outcome.stderr);
    let evidence = TestEvidence {
        baseline: before.baseline,
        head: before.head,
        changed_paths: before.changed_paths,
        file_digests: before.file_digests,
        verifier_identity: verifier_identity.to_owned(),
        exit_code: outcome.returncode,
        output_sha256: hex::encode(hashed.finalize()),
    };
    Ok(VerificationRun {
        evidence,
        stdout: String::from_utf8_lossy(&outcome.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&outcome.stderr).into_owned(),
        timed_out: outcome.timed_out,
    })
}

/// Run allowlisted pytest and evaluate snapshot-bound evidence without merging.
///
/// # Errors
/// Returns [`VerificationError`] without authorization or trusted code, or when the run or
/// the evidence evaluation fails.
pub fn verify_and_evaluate(
    workspace: &Path,
    baseline: &str,
    owned_paths: &[String],
    verifier_identity: &str,
    options: VerifyOptions,
) -> Result<(VerificationRun, VerificationDecision), VerificationError> {
    if !options.authorized {
        return Err(VerificationError::msg(
            "Explicit verification authorization required",
        ));
    }
    if !options.trusted_code {
        return Err(VerificationError::msg(
            "Untrusted code must use a qualified isolated verifier",
        ));
    }
    let manifest = prepare_verification(workspace, baseline, owned_paths)?;
    let result = run_verification(
        workspace,
        baseline,
        owned_paths,
        verifier_identity,
        "pytest",
        VerifyOptions {
            authorized: true,
            ..options
        },
    )?;
    let decision = evaluate_test_evidence(&manifest, &result.evidence)?;
    Ok((result, decision))
}
