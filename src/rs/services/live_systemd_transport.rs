//! Restricted systemd transport for pre-reviewed sandbox plans.
//!
//! Constructing this transport does not authorize execution. Only an explicitly
//! injected subprocess runner can perform operations, and no shell is involved.

use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::sandbox::live_sandbox_plan::SandboxPlan;
use crate::services::live_service_controller::{ServiceCommandTransport, TransportError};
use crate::util::is_py_space;

const RUN: &str = "/usr/bin/systemd-run";
const CTL: &str = "/usr/bin/systemctl";

/// Systemd operation failed or returned an untrusted response.
#[derive(Debug, thiserror::Error)]
pub enum SystemdTransportError {
    /// A transport rule was violated or a command failed.
    #[error("{0}")]
    Refused(&'static str),
    /// The host command could not be spawned or read.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result of an injected runner (`subprocess.CompletedProcess[str]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedProcess {
    /// Exit status (negative signal number when killed by a signal).
    pub returncode: i32,
    /// Captured standard output.
    pub stdout: String,
}

/// Injected command runner.
pub type Runner = Box<dyn Fn(&[String]) -> Result<CompletedProcess, SystemdTransportError>>;

/// Default runner: refuses every command.
///
/// # Errors
/// Always returns an error because host command execution is disabled.
pub const fn deny_runner(_argv: &[String]) -> Result<CompletedProcess, SystemdTransportError> {
    Err(SystemdTransportError::Refused(
        "Host commands disabled: inject a qualified operator runner explicitly",
    ))
}

fn unit_is_valid(unit: &str) -> bool {
    unit.strip_prefix("hermes-job-").is_some_and(|rest| {
        (1..=53).contains(&rest.len())
            && rest
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
    })
}

/// Operate exactly one approved unit and exact launch/stop arguments.
///
/// Caller must separately qualify privileged invocation, filesystem mounts,
/// gateway connectivity and process isolation before injecting a real runner.
pub struct RestrictedSystemdTransport {
    /// The approved plan.
    plan: SandboxPlan,
    runner: Runner,
}

impl std::fmt::Debug for RestrictedSystemdTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestrictedSystemdTransport")
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}

impl RestrictedSystemdTransport {
    /// The validated sandbox plan.
    #[must_use]
    pub const fn plan(&self) -> &SandboxPlan {
        &self.plan
    }

    /// Transport with the default deny-all runner.
    ///
    /// # Errors
    /// The plan has an invalid service unit, launch command, or stop command.
    pub fn new(plan: SandboxPlan) -> Result<Self, SystemdTransportError> {
        Self::with_runner(plan, Box::new(deny_runner))
    }

    /// Transport with an explicitly injected runner.
    ///
    /// # Errors
    /// The plan has an invalid service unit, launch command, or stop command.
    pub fn with_runner(plan: SandboxPlan, runner: Runner) -> Result<Self, SystemdTransportError> {
        if !unit_is_valid(&plan.unit) {
            return Err(SystemdTransportError::Refused("Invalid service unit"));
        }
        if plan.systemd_argv.first().map(String::as_str) != Some(RUN) {
            return Err(SystemdTransportError::Refused(
                "Expected systemd-run launch plan",
            ));
        }
        let expected_stop = [CTL, "stop", &format!("{}.service", plan.unit)];
        if plan.stop_argv != expected_stop {
            return Err(SystemdTransportError::Refused(
                "Unexpected service stop command",
            ));
        }
        Ok(Self { plan, runner })
    }

    fn run(&self, argv: &[String]) -> Result<String, SystemdTransportError> {
        let completed = (self.runner)(argv)?;
        if completed.returncode != 0 {
            return Err(SystemdTransportError::Refused(
                "Systemd command failed; outcome must be reconciled without replay",
            ));
        }
        Ok(completed.stdout)
    }

    /// Only invoke the exact audited launch or stop vector.
    ///
    /// # Errors
    /// The unit or command violates the approved plan, or the injected runner fails.
    pub fn invoke(&self, argv: &[String]) -> Result<(), SystemdTransportError> {
        if argv != self.plan.systemd_argv && argv != self.plan.stop_argv {
            return Err(SystemdTransportError::Refused(
                "Command is not part of the authorized sandbox plan",
            ));
        }
        self.run(argv).map(|_| ())
    }

    fn property(&self, unit: &str, name: &str) -> Result<String, SystemdTransportError> {
        if unit != self.plan.unit {
            return Err(SystemdTransportError::Refused(
                "Cross-unit inspection denied",
            ));
        }
        let argv: Vec<String> = [CTL, "show", "--property", name, "--value"]
            .iter()
            .map(|value| (*value).to_owned())
            .chain([format!("{unit}.service")])
            .collect();
        Ok(self.run(&argv)?.trim_matches(is_py_space).to_owned())
    }

    /// Require both active state and a live main process.
    ///
    /// # Errors
    /// The unit or command violates the approved plan, or the injected runner fails.
    pub fn is_active(&self, unit: &str) -> Result<bool, SystemdTransportError> {
        if self.property(unit, "ActiveState")? != "active" {
            return Ok(false);
        }
        let pid = self.property(unit, "MainPID")?;
        Ok(!pid.is_empty()
            && pid.bytes().all(|c| c.is_ascii_digit())
            && pid.bytes().any(|c| c != b'0'))
    }

    /// Require stopped service and no main process.
    ///
    /// # Errors
    /// The unit or command violates the approved plan, or the injected runner fails.
    pub fn is_inactive(&self, unit: &str) -> Result<bool, SystemdTransportError> {
        Ok(self.property(unit, "ActiveState")? == "inactive"
            && self.property(unit, "MainPID")? == "0")
    }
}

impl ServiceCommandTransport for RestrictedSystemdTransport {
    fn invoke(&self, argv: &[String]) -> Result<(), TransportError> {
        Self::invoke(self, argv).map_err(Into::into)
    }

    fn is_active(&self, unit: &str) -> Result<bool, TransportError> {
        Self::is_active(self, unit).map_err(Into::into)
    }

    fn is_inactive(&self, unit: &str) -> Result<bool, TransportError> {
        Self::is_inactive(self, unit).map_err(Into::into)
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
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

fn drain<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    })
}

/// Opt-in host runner; caller must explicitly inject this after qualification.
///
/// Never elevate privileges, spawn a shell, or assume timeout means failure.
///
/// # Errors
/// The unit or command violates the approved plan, or the injected runner fails.
pub fn operator_subprocess_runner(
    argv: &[String],
) -> Result<CompletedProcess, SystemdTransportError> {
    let Some((program, rest)) = argv.split_first() else {
        return Err(SystemdTransportError::Io(std::io::Error::other(
            "empty argument vector",
        )));
    };
    let mut child = Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out_reader = drain(child.stdout.take());
    let err_reader = drain(child.stderr.take());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = out_reader.join().unwrap_or_default();
    let _ = err_reader.join();
    let Some(status) = status else {
        return Err(SystemdTransportError::Refused(
            "Systemd request timed out; outcome unknown, do not resubmit",
        ));
    };
    Ok(CompletedProcess {
        returncode: exit_code(status),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
    })
}
