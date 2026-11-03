//! Supervised foreground fixture transport; live model executables remain disabled.

use crate::util::create_private_dir;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::watchdog::{Budget, Outcome, SupervisionOptions, WatchdogError, supervise};
use crate::gateway::hermes_adapter::PreparedInvocation;
use crate::util::{PyStr as _, ordered_json_spaced, uuid4_hex};

/// Largest accepted fixture output in bytes.
pub const MAX_OUTPUT_BYTES: usize = 40_000;
/// Executable names that are never run by the fixture transport.
pub const FORBIDDEN_EXECUTABLES: [&str; 3] = ["hermes", "codex", "ollama"];

/// Errors raised by the fixture transport.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Foreground fixture failed or violated its execution contract.
    #[error("{0}")]
    Transport(String),
    /// Invalid argument (Python `ValueError`).
    #[error("{0}")]
    Value(String),
    /// Forbidden executable (Python `PermissionError`).
    #[error("{0}")]
    Permission(String),
    /// Supervision failure.
    #[error(transparent)]
    Watchdog(#[from] WatchdogError),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] crate::util::JsonError),
}

fn fail<T>(msg: &str) -> Result<T, TransportError> {
    Err(TransportError::Transport(msg.to_owned()))
}

/// Runner selected by trusted harness code, never by a model response.
#[derive(Debug, Clone, PartialEq)]
pub struct ForegroundOptions {
    /// Absolute executable followed by fixed arguments.
    pub argv: Vec<String>,
    /// Directory receiving per-run records.
    pub records: PathBuf,
    /// Timing budgets.
    pub budget: Budget,
}

impl ForegroundOptions {
    /// Build options with the default budget.
    #[must_use]
    pub fn new(argv: Vec<String>, records: impl Into<PathBuf>) -> Self {
        Self {
            argv,
            records: records.into(),
            budget: Budget::default(),
        }
    }

    /// Reject relative, live-model, or missing executables and bad budgets.
    ///
    /// # Errors
    /// The executable is relative, missing, or forbidden, or the execution budget is invalid.
    pub fn validate(&self) -> Result<(), TransportError> {
        let executable = self.argv.first().map(Path::new);
        let Some(executable) = executable.filter(|path| path.is_absolute()) else {
            return Err(TransportError::Value(
                "Fixture executable must use an absolute path".to_owned(),
            ));
        };
        let name = executable
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if FORBIDDEN_EXECUTABLES.contains(&name.as_str()) {
            return Err(TransportError::Permission(
                "Live model executables are not supported by fixture transport".to_owned(),
            ));
        }
        if !executable.is_file() {
            return Err(TransportError::Value(
                "Fixture executable does not exist".to_owned(),
            ));
        }
        Ok(self.budget.validate()?)
    }
}

/// Protocol-compatible worker for trusted fixtures, not live inference.
#[derive(Debug)]
pub struct ForegroundFixtureWorker<'a> {
    options: ForegroundOptions,
    cancel: Option<&'a AtomicBool>,
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

const fn outcome_name(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Completed => "completed",
        Outcome::TimedOut => "timed_out",
        Outcome::Cancelled => "cancelled",
        Outcome::CleanupFailed => "cleanup_failed",
    }
}

impl<'a> ForegroundFixtureWorker<'a> {
    /// Validate the options and build a worker; `cancel` stops a running fixture.
    ///
    /// # Errors
    /// The fixture executable or execution budget fails validation.
    pub fn new(
        options: ForegroundOptions,
        cancel: Option<&'a AtomicBool>,
    ) -> Result<Self, TransportError> {
        options.validate()?;
        Ok(Self { options, cancel })
    }

    fn write_request(
        &self,
        invocation: &PreparedInvocation,
    ) -> Result<(PathBuf, PathBuf), TransportError> {
        if invocation.execution_enabled {
            return fail("Live-enabled invocations are prohibited");
        }
        let workspace = fs::canonicalize(&invocation.workspace)?;
        if !workspace.is_dir() {
            return fail("Workspace is not a directory");
        }
        fs::create_dir_all(&self.options.records)?;
        let directory = self
            .options
            .records
            .join(format!("fixture-{}", uuid4_hex()?));
        create_private_dir(&directory)?;
        let request = directory.join("request.json");
        let mut stream = create_private_file(&request)?;
        stream.write_all(ordered_json_spaced(invocation)?.as_bytes())?;
        Ok((workspace, request))
    }

    #[allow(clippy::cast_precision_loss)] // deadlines are small whole seconds
    fn execute(
        &self,
        workspace: &Path,
        request: &Path,
        deadline: i64,
    ) -> Result<PathBuf, TransportError> {
        let budget = &self.options.budget;
        let effective = Budget {
            deadline_seconds: budget.deadline_seconds.min(deadline as f64),
            grace_seconds: budget.grace_seconds,
            poll_seconds: budget.poll_seconds,
        };
        let mut argv = self.options.argv.clone();
        argv.push(request.to_string_lossy().into_owned());
        let records = request.parent().map(Path::to_path_buf).unwrap_or_default();
        let options = SupervisionOptions {
            cwd: workspace.to_path_buf(),
            records,
            budget: effective,
        };
        let record = supervise(&argv, &options, self.cancel)?;
        if record.outcome != Outcome::Completed
            || record.returncode != Some(0)
            || record.group_alive
        {
            return fail(&format!(
                "Fixture terminated without success: {}",
                outcome_name(record.outcome)
            ));
        }
        Ok(PathBuf::from(record.stdout_path))
    }

    fn read_output(output: &Path) -> Result<String, TransportError> {
        let limit = u64::try_from(MAX_OUTPUT_BYTES + 1).unwrap_or(u64::MAX);
        let mut payload = Vec::new();
        File::open(output)?.take(limit).read_to_end(&mut payload)?;
        if payload.len() > MAX_OUTPUT_BYTES {
            return fail("Fixture output exceeds byte budget");
        }
        let Ok(result) = String::from_utf8(payload) else {
            return fail("Fixture emitted invalid UTF-8");
        };
        if result.py_strip().is_empty() {
            return fail("Fixture output is empty");
        }
        Ok(result)
    }

    /// Run a trusted fixture using the bounded watchdog and return its text.
    ///
    /// # Errors
    /// Fixture options or invocation are invalid, file I/O fails, or supervised execution does not
    /// complete successfully.
    pub fn run(&self, invocation: &PreparedInvocation) -> Result<String, TransportError> {
        let (workspace, request) = self.write_request(invocation)?;
        let output = self.execute(&workspace, &request, invocation.deadline_seconds)?;
        Self::read_output(&output)
    }
}
