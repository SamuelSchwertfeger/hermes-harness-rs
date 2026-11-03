//! Opt-in, provider-free host verification for existing systemd/Bubblewrap controls.
//!
//! This module does not authorize live gateway traffic. It only executes a fixed
//! /bin/sleep service inside the operator-selected qualified sandbox profile.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Read, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::util::{JsonError, pretty_json};

const PROPERTIES: [&str; 8] = [
    "ActiveState",
    "MainPID",
    "User",
    "MemoryMax",
    "CPUQuotaPerSecUSec",
    "TasksMax",
    "KillMode",
    "ControlGroup",
];
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Verification failed; never infer live runtime qualification.
#[derive(Debug, thiserror::Error)]
pub enum HostProbeError {
    /// A probe rule was violated.
    #[error("{0}")]
    Refused(String),
    /// Filesystem or process failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

fn refuse<T>(message: &str) -> Result<T, HostProbeError> {
    Err(HostProbeError::Refused(message.to_owned()))
}

/// One non-authoritative host observation, safe to save as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeObservation {
    /// Verification unit name.
    pub unit: String,
    /// Allowlisted systemd property values.
    pub values: BTreeMap<String, String>,
    /// Control-group PIDs seen before stop.
    pub cgroup_pids_before_stop: Vec<i64>,
    /// PIDs still present after stop.
    pub pids_remaining_after_stop: Vec<i64>,
    /// Final unit state.
    pub final_state: String,
    /// Always false unless a qualified operator sets it elsewhere.
    pub qualified: bool,
}

impl ProbeObservation {
    /// Build an unqualified observation.
    #[must_use]
    pub fn new(
        unit: &str,
        values: BTreeMap<String, String>,
        cgroup_pids_before_stop: Vec<i64>,
        pids_remaining_after_stop: Vec<i64>,
        final_state: &str,
    ) -> Self {
        Self {
            unit: unit.to_owned(),
            values,
            cgroup_pids_before_stop,
            pids_remaining_after_stop,
            final_state: final_state.to_owned(),
            qualified: false,
        }
    }

    /// Print allowlisted evidence only, not process arguments or secrets.
    ///
    /// # Errors
    /// The value cannot be serialized by the Python-compatible JSON encoder.
    pub fn to_json(&self) -> Result<String, JsonError> {
        pretty_json(self)
    }
}

/// Completed host command (`subprocess.CompletedProcess[str]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit status.
    pub returncode: i32,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Injected host command runner.
pub type Runner<'a> = &'a dyn Fn(&[String]) -> Result<CommandOutput, HostProbeError>;

/// Default runner: host execution is disabled.
///
/// # Errors
/// Always returns an error because host probing requires an injected operator runner.
pub fn disabled(_argv: &[String]) -> Result<CommandOutput, HostProbeError> {
    refuse("Host execution disabled; explicitly provide a qualified operator runner")
}

fn drain<R: Read + Send + 'static>(mut stream: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stream.read_to_end(&mut buffer);
        String::from_utf8_lossy(&buffer).into_owned()
    })
}

/// Explicit opt-in, non-shell host runner; no sudo or provider invocation.
///
/// # Errors
/// Host probing is disabled, command execution fails, or the observed service evidence is invalid.
pub fn operator_runner(argv: &[String]) -> Result<CommandOutput, HostProbeError> {
    let Some((program, rest)) = argv.split_first() else {
        return refuse("Empty host command");
    };
    let mut child = Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return refuse("Host observation timed out; outcome uncertain");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let collect = |handle: Option<thread::JoinHandle<String>>| {
        handle.and_then(|h| h.join().ok()).unwrap_or_default()
    };
    Ok(CommandOutput {
        returncode: status.code().unwrap_or(-1),
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// Fixed, operator-inspectable host commands with no arbitrary argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeCommands {
    /// Disposable verification unit (without `.service`).
    pub unit: String,
}

fn valid_unit(unit: &str) -> bool {
    unit.strip_prefix("hermes-verify-").is_some_and(|rest| {
        (8..=24).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

impl ProbeCommands {
    /// Validate that `unit` is inside the disposable verification namespace.
    ///
    /// # Errors
    /// The unit name or command configuration violates the host probe restrictions.
    pub fn new(unit: &str) -> Result<Self, HostProbeError> {
        if !valid_unit(unit) {
            return refuse("Unit identity is outside the disposable verification namespace");
        }
        Ok(Self {
            unit: unit.to_owned(),
        })
    }

    /// Read a single allowlisted unit property.
    ///
    /// # Errors
    /// Host probing is disabled, command execution fails, or the observed service evidence is
    /// invalid.
    pub fn show(&self, name: &str) -> Result<Vec<String>, HostProbeError> {
        if !PROPERTIES.contains(&name) {
            return refuse("Unrecognized systemd property");
        }
        Ok([
            "/usr/bin/systemctl",
            "show",
            "--property",
            name,
            "--value",
            &format!("{}.service", self.unit),
        ]
        .map(str::to_owned)
        .to_vec())
    }

    /// Stop only the exact verified unit.
    #[must_use]
    pub fn stop(&self) -> Vec<String> {
        [
            "/usr/bin/systemctl",
            "stop",
            &format!("{}.service", self.unit),
        ]
        .map(str::to_owned)
        .to_vec()
    }
}

fn invoke(runner: Runner<'_>, command: &[String]) -> Result<String, HostProbeError> {
    let result = runner(command)?;
    if result.returncode != 0 {
        return refuse("Host command failed; stop or launch outcome is not assumed");
    }
    Ok(crate::util::py_strip(&result.stdout).to_owned())
}

/// Read effective systemd state without modifying the service.
///
/// # Errors
/// Host probing is disabled, command execution fails, or the observed service evidence is invalid.
pub fn observe_unit(
    commands: &ProbeCommands,
    runner: Runner<'_>,
) -> Result<BTreeMap<String, String>, HostProbeError> {
    PROPERTIES
        .iter()
        .map(|key| Ok(((*key).to_owned(), invoke(runner, &commands.show(key)?)?)))
        .collect()
}

/// Validate exact limits and identity; never grant gateway authorization.
#[must_use]
pub fn assess_observation(values: &BTreeMap<String, String>) -> Vec<String> {
    let expected = [
        ("ActiveState", "active"),
        ("User", "hermes"),
        ("MemoryMax", "536870912"),
        ("CPUQuotaPerSecUSec", "500ms"),
        ("TasksMax", "32"),
        ("KillMode", "control-group"),
    ];
    let mut failures: Vec<String> = expected
        .iter()
        .filter(|(name, value)| values.get(*name).map(String::as_str) != Some(*value))
        .map(|(name, _)| (*name).to_owned())
        .collect();
    let pid = values.get("MainPID").map_or("", String::as_str);
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) || pid.bytes().all(|b| b == b'0')
    {
        failures.push("MainPID".to_owned());
    }
    let group = values.get("ControlGroup").map_or("", String::as_str);
    if !group.starts_with("/system.slice/hermes-verify-") || group.contains("..") {
        failures.push("ControlGroup".to_owned());
    }
    failures
}

/// Reject a snapshot with no actual processes or with surviving descendants.
///
/// # Errors
/// The PID snapshot is malformed or does not match the expected process identity.
pub fn validate_pid_snapshot(pids: &[i64], remaining: &[i64]) -> Result<(), HostProbeError> {
    let mut unique = pids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if pids.is_empty() || pids.iter().any(|pid| *pid <= 1) || unique.len() != pids.len() {
        return refuse("Invalid or empty host process snapshot");
    }
    if !remaining.is_empty() {
        return refuse("Processes remained after service termination");
    }
    Ok(())
}

/// Create evidence exclusively; never overwrite or follow symlink targets.
///
/// # Errors
/// The evidence directory or file cannot be created, encoded, or written.
pub fn write_probe_evidence(
    path: &Path,
    observation: &ProbeObservation,
) -> Result<(), HostProbeError> {
    let is_symlink = |p: &Path| crate::util::lstat(p).is_ok_and(|m| m.is_symlink());
    if is_symlink(path) || path.exists() || path.parent().is_some_and(is_symlink) {
        return refuse("Evidence path already exists or uses a symlink");
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().try_into().unwrap_or(0));
    }
    let mut stream = options.open(path)?;
    stream.write_all(format!("{}\n", observation.to_json()?).as_bytes())?;
    stream.flush()?;
    stream.sync_all()?;
    Ok(())
}
