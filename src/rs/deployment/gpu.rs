//! Conservative, operator-authorized GPU slot leasing and systemd switching.
use crate::util::PathLexical as _;
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::multiagent::runtime::WorkflowError;
use crate::util::{pretty_json, pretty_json_value, py_strip, sha256_hex};

/// Maximum policy size.
pub const MAX_POLICY_SIZE: u64 = 32_768;
/// Operator-owned service estimate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GPUUnit {
    /// Service name.
    pub name: String,
    /// Memory estimate.
    pub vram_mib: i64,
    /// Must remain resident.
    pub pinned: bool,
    /// May be switched.
    pub managed: bool,
    /// Expected model.
    pub model: String,
}
/// Host budget and approved services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GPUPolicy {
    /// Owning host.
    pub host: String,
    /// Physical capacity.
    pub capacity_mib: i64,
    /// Reserved headroom.
    pub reserved_mib: i64,
    /// Shared lease file.
    pub lock_path: PathBuf,
    /// Approved services.
    pub units: Vec<GPUUnit>,
}
fn require(ok: bool, message: &str) -> Result<(), WorkflowError> {
    if ok {
        Ok(())
    } else {
        Err(WorkflowError::msg(message))
    }
}
impl GPUPolicy {
    /// Validate capacities and service identities.
    /// # Errors
    /// Rejects invalid operator policies.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        require(
            !self.host.is_empty() && self.capacity_mib > 0 && self.reserved_mib >= 0,
            "Invalid GPU policy capacities",
        )?;
        require(
            self.reserved_mib < self.capacity_mib,
            "GPU reserve exceeds physical capacity",
        )?;
        require(
            !self.lock_path.is_symlink_lexical()
                && self
                    .lock_path
                    .parent()
                    .map(|p| {
                        if p.as_os_str().is_empty() {
                            Path::new(".")
                        } else {
                            p
                        }
                    })
                    .is_some_and(Path::is_dir),
            "GPU lock directory unavailable",
        )?;
        let names: BTreeSet<_> = self.units.iter().map(|u| &u.name).collect();
        require(
            names.len() == self.units.len(),
            "Duplicate GPU service identity",
        )?;
        for unit in &self.units {
            require(
                unit.name.ends_with(".service") && !unit.name.contains('/') && unit.vram_mib > 0,
                "Invalid GPU unit or memory estimate",
            )?;
        }
        let pinned: i128 = self
            .units
            .iter()
            .filter(|u| u.pinned)
            .map(|u| i128::from(u.vram_mib))
            .sum();
        require(
            pinned + i128::from(self.reserved_mib) <= i128::from(self.capacity_mib),
            "Pinned services exceed GPU budget",
        )
    }
    /// Find an approved service.
    /// # Errors
    /// Rejects unregistered services.
    pub fn unit(&self, name: &str) -> Result<&GPUUnit, WorkflowError> {
        self.units
            .iter()
            .find(|u| u.name == name)
            .ok_or_else(|| WorkflowError::msg("GPU service not in operator policy"))
    }
    /// Admit selected and pinned services.
    /// # Errors
    /// Rejects overcommit and invalid policies.
    pub fn admit(&self, names: &[&str]) -> Result<(), WorkflowError> {
        self.validate()?;
        let mut selected = BTreeSet::new();
        for name in names {
            selected.insert(self.unit(name)?.name.as_str());
        }
        selected.extend(
            self.units
                .iter()
                .filter(|u| u.pinned)
                .map(|u| u.name.as_str()),
        );
        let mut budget = i128::from(self.reserved_mib);
        for name in selected {
            budget += i128::from(self.unit(name)?.vram_mib);
        }
        require(
            budget <= i128::from(self.capacity_mib),
            "GPU VRAM reservation would overcommit",
        )
    }
}
/// Load a bounded operator policy.
/// # Errors
/// Rejects symlinks and malformed policies.
pub fn load_gpu_policy(path: &Path) -> Result<GPUPolicy, WorkflowError> {
    require(
        !path.is_symlink_lexical() && path.is_file() && path.metadata()?.len() <= MAX_POLICY_SIZE,
        "Invalid GPU policy file",
    )?;
    let policy: GPUPolicy = serde_json::from_slice(
        &std::fs::read(path).map_err(|_| WorkflowError::msg("Malformed GPU policy"))?,
    )
    .map_err(|_| WorkflowError::msg("Malformed GPU policy"))?;
    policy.validate()?;
    Ok(policy)
}
/// Closing the guard releases the lease.
#[derive(Debug)]
pub struct GpuLane {
    _file: File,
    #[cfg(not(unix))]
    _guard: crate::util::process_lock::Guard,
}
/// Acquire one nonblocking lease.
/// # Errors
/// Rejects invalid policies or busy lanes.
pub fn gpu_lane(policy: &GPUPolicy) -> Result<GpuLane, WorkflowError> {
    policy.validate()?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(
            i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).map_err(std::io::Error::other)?,
        );
    }
    let file = options.open(&policy.lock_path)?;
    #[cfg(unix)]
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
        |error| {
            if error == rustix::io::Errno::WOULDBLOCK {
                WorkflowError::msg("GPU lane busy; refuse overlapping dispatch or switch")
            } else {
                WorkflowError::Io(error.into())
            }
        },
    )?;
    #[cfg(not(unix))]
    let guard = crate::util::process_lock::try_acquire(&std::fs::canonicalize(&policy.lock_path)?)
        .ok_or_else(|| {
            WorkflowError::msg("GPU lane busy; refuse overlapping dispatch or switch")
        })?;
    Ok(GpuLane {
        _file: file,
        #[cfg(not(unix))]
        _guard: guard,
    })
}
/// Minimal service-control seam.
pub trait Systemd {
    /// Observe state.
    /// # Errors
    /// Returns service observation failures.
    fn active(&mut self, unit: &str) -> Result<bool, WorkflowError>;
    /// Start approved service.
    /// # Errors
    /// Returns service start failures.
    fn start(&mut self, unit: &str) -> Result<(), WorkflowError>;
    /// Stop approved service.
    /// # Errors
    /// Returns service stop failures.
    fn stop(&mut self, unit: &str) -> Result<(), WorkflowError>;
}
/// Captured probe response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutput {
    /// Exit status.
    pub returncode: i32,
    /// Standard output.
    pub stdout: String,
}
/// Injectable subprocess seam.
pub trait ProbeRunner {
    /// Host name, replaceable by offline runners.
    /// # Errors
    /// Returns hostname command failures.
    fn hostname(&self) -> Result<String, WorkflowError> {
        Ok(py_strip(&self.run(&["hostname"], Duration::from_secs(8))?.stdout).to_owned())
    }

    /// Execute exact argv with a timeout.
    /// # Errors
    /// Returns process and timeout failures.
    fn run(&self, argv: &[&str], timeout: Duration) -> Result<ProbeOutput, WorkflowError>;
}
/// Real runner without shell execution.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommandRunner;
impl ProbeRunner for CommandRunner {
    #[cfg(unix)]
    fn hostname(&self) -> Result<String, WorkflowError> {
        Ok(rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned())
    }

    fn run(&self, argv: &[&str], timeout: Duration) -> Result<ProbeOutput, WorkflowError> {
        let (program, arguments) = argv
            .split_first()
            .ok_or_else(|| WorkflowError::msg("Empty probe argv"))?;
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let output = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stream) = stdout.as_mut() {
                let _ = stream.read_to_end(&mut bytes);
            }
            bytes
        });
        let errors = std::thread::spawn(move || {
            if let Some(stream) = stderr.as_mut() {
                let _ = std::io::copy(stream, &mut std::io::sink());
            }
        });
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if start.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output.join();
                let _ = errors.join();
                return Err(WorkflowError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Probe timed out",
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let bytes = output.join().unwrap_or_default();
        let _ = errors.join();
        Ok(ProbeOutput {
            returncode: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}
/// No sudo, shell, or enable/disable operations.
#[derive(Debug, Clone, Default)]
pub struct UserSystemd<R = CommandRunner> {
    /// Injectable runner.
    pub runner: R,
}
impl<R: ProbeRunner> UserSystemd<R> {
    fn run(&self, operation: &str, unit: &str) -> Result<ProbeOutput, WorkflowError> {
        self.runner.run(
            &["/usr/bin/systemctl", operation, unit],
            Duration::from_secs(25),
        )
    }
}
impl<R: ProbeRunner> Systemd for UserSystemd<R> {
    fn active(&mut self, unit: &str) -> Result<bool, WorkflowError> {
        Ok(self.run("is-active", unit)?.returncode == 0)
    }
    fn start(&mut self, unit: &str) -> Result<(), WorkflowError> {
        require(
            self.run("start", unit)?.returncode == 0,
            "Approved GPU service start failed",
        )
    }
    fn stop(&mut self, unit: &str) -> Result<(), WorkflowError> {
        require(
            self.run("stop", unit)?.returncode == 0,
            "Approved GPU service stop failed",
        )
    }
}
/// Minimal native evidence-writing seam; the native repair controller is a phase-5 stub.
pub trait EvidenceWriter {
    /// Exclusively create and durably flush evidence, without overwriting existing files.
    /// # Errors
    /// Returns filesystem, size, or exclusive-create failures.
    fn write(&self, path: &Path, payload: &[u8]) -> Result<(), WorkflowError>;
}
/// Filesystem implementation of the native write-once evidence contract.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileEvidenceWriter;
impl EvidenceWriter for FileEvidenceWriter {
    fn write(&self, path: &Path, payload: &[u8]) -> Result<(), WorkflowError> {
        let parent = path
            .parent()
            .map(|p| {
                if p.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    p
                }
            })
            .ok_or_else(|| WorkflowError::msg("Invalid evidence directory"))?;
        require(
            parent.is_dir() && !parent.is_symlink_lexical(),
            "Invalid evidence directory",
        )?;
        require(payload.len() <= 128 * 1024, "Evidence too large")?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(
                i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits())
                    .map_err(std::io::Error::other)?,
            );
        }
        let mut file = options.open(path)?;
        file.write_all(payload)?;
        file.sync_all()?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}
pub(crate) fn write_once(path: &Path, value: &impl Serialize) -> Result<String, WorkflowError> {
    write_once_with(path, value, &FileEvidenceWriter)
}

/// Write an existing JSON tree as pretty evidence with a trailing newline.
pub(crate) fn write_once_value(path: &Path, value: &Value) -> Result<String, WorkflowError> {
    write_value_with(path, value, &FileEvidenceWriter)
}

fn write_value_with(
    path: &Path,
    value: &Value,
    writer: &dyn EvidenceWriter,
) -> Result<String, WorkflowError> {
    write_payload(path, &(pretty_json_value(value)? + "\n"), writer)
}
fn write_once_with(
    path: &Path,
    value: &impl Serialize,
    writer: &dyn EvidenceWriter,
) -> Result<String, WorkflowError> {
    write_payload(path, &(pretty_json(value)? + "\n"), writer)
}

fn write_payload(
    path: &Path,
    payload: &str,
    writer: &dyn EvidenceWriter,
) -> Result<String, WorkflowError> {
    writer.write(path, payload.as_bytes())?;
    Ok(sha256_hex(payload.as_bytes()))
}
/// Explicit switch request.
#[derive(Debug, Clone, Copy)]
pub struct SwitchRequest<'a> {
    /// Policy.
    pub policy: &'a GPUPolicy,
    /// Active unit.
    pub prior: &'a str,
    /// Desired unit.
    pub target: &'a str,
    /// One-shot identity.
    pub transaction: &'a str,
    /// Existing journal directory.
    pub journal: &'a Path,
    /// Separate authorization.
    pub authorized: bool,
}
fn switch_validate(request: SwitchRequest<'_>) -> Result<&GPUUnit, WorkflowError> {
    require(
        request.authorized,
        "Explicit host model-switch authorization required",
    )?;
    let old = request.policy.unit(request.prior)?;
    let new = request.policy.unit(request.target)?;
    require(
        !old.pinned && old.managed && new.managed,
        "Pinned or unmanaged service may not be switched",
    )?;
    require(old.name != new.name, "No-op GPU switch not permitted")?;
    request.policy.admit(&[request.target])?;
    require(
        request.journal.is_dir() && !request.journal.is_symlink_lexical(),
        "Invalid GPU switch journal",
    )?;
    require(
        !request.transaction.is_empty()
            && request
                .transaction
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
        "Invalid transaction identifier",
    )?;
    Ok(new)
}
fn perform_switch(
    request: SwitchRequest<'_>,
    new: &GPUUnit,
    systemd: &mut dyn Systemd,
    ready: &mut dyn FnMut(&GPUUnit) -> Result<bool, WorkflowError>,
    free: Option<&mut dyn FnMut() -> Result<i64, WorkflowError>>,
) -> Result<(), WorkflowError> {
    systemd.stop(request.prior)?;
    require(
        !systemd.active(request.prior)?,
        "Prior GPU service did not drain",
    )?;
    if let Some(free) = free {
        require(
            i128::from(free()?)
                >= i128::from(new.vram_mib) + i128::from(request.policy.reserved_mib),
            "Insufficient observed GPU free memory for switch",
        )?;
    }
    systemd.start(request.target)?;
    require(
        systemd.active(request.target)? && ready(new)?,
        "Target GPU model not authenticated and ready",
    )
}
/// Immutable switch result in Python field order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SwitchResult {
    /// Record mode.
    pub mode: &'static str,
    /// Digest of the original claim.
    pub claim_sha256: String,
    /// READY or UNKNOWN.
    pub phase: &'static str,
    /// Operator recovery required.
    pub manual_reconciliation_required: bool,
}
/// One-shot switch; failures require reconciliation.
/// # Errors
/// Rejects unauthorized requests, replay, or UNKNOWN outcomes.
pub fn switch_gpu_slot(
    request: SwitchRequest<'_>,
    systemd: &mut dyn Systemd,
    ready: &mut dyn FnMut(&GPUUnit) -> Result<bool, WorkflowError>,
    free: Option<&mut dyn FnMut() -> Result<i64, WorkflowError>>,
) -> Result<SwitchResult, WorkflowError> {
    switch_gpu_slot_with_writer(request, systemd, ready, free, &FileEvidenceWriter)
}
/// Switch GPU slots with caller-supplied service, readiness and evidence operations.
/// # Errors
/// Rejects unauthorized requests, replay, evidence failures, or UNKNOWN outcomes.
pub fn switch_gpu_slot_with_writer(
    request: SwitchRequest<'_>,
    systemd: &mut dyn Systemd,
    ready: &mut dyn FnMut(&GPUUnit) -> Result<bool, WorkflowError>,
    free: Option<&mut dyn FnMut() -> Result<i64, WorkflowError>>,
    writer: &dyn EvidenceWriter,
) -> Result<SwitchResult, WorkflowError> {
    let new = switch_validate(request)?;
    let _lane = gpu_lane(request.policy)?;
    require(
        systemd.active(request.prior)? && !systemd.active(request.target)?,
        "Unqualified initial GPU service state",
    )?;
    let claim = json!({"mode":"gpu-switch-claim", "host":request.policy.host, "prior":request.prior, "target":request.target, "transaction":request.transaction, "phase":"UNKNOWN", "operator_authorized":true});
    let claim_sha = write_value_with(
        &request
            .journal
            .join(format!("{}-claim.json", request.transaction)),
        &claim,
        writer,
    )
    .map_err(|error| {
        if error.is_already_exists() {
            WorkflowError::msg("GPU transaction already claimed; do not replay")
        } else {
            error
        }
    })?;
    let failed = perform_switch(request, new, systemd, ready, free).is_err();
    let result = SwitchResult {
        mode: "gpu-switch-result",
        claim_sha256: claim_sha,
        phase: if failed { "UNKNOWN" } else { "READY" },
        manual_reconciliation_required: failed,
    };
    write_once_with(
        &request
            .journal
            .join(format!("{}-result.json", request.transaction)),
        &result,
        writer,
    )?;
    require(
        !failed,
        "GPU switch outcome UNKNOWN; operator recovery required",
    )?;
    Ok(result)
}
/// Observe free memory on one NVIDIA GPU.
/// # Errors
/// Rejects failed or invalid observations.
pub fn observed_free_vram_mib() -> Result<i64, WorkflowError> {
    observed_free_vram_mib_with(&CommandRunner)
}
/// Observe free VRAM with a caller-supplied probe runner.
/// # Errors
/// Rejects failed or invalid observations.
pub fn observed_free_vram_mib_with(runner: &dyn ProbeRunner) -> Result<i64, WorkflowError> {
    let response = runner.run(
        &[
            "/usr/bin/nvidia-smi",
            "--query-gpu=memory.free",
            "--format=csv,noheader,nounits",
        ],
        Duration::from_secs(8),
    )?;
    require(
        response.returncode == 0 && response.stdout.lines().count() == 1,
        "NVIDIA GPU free-memory observation unavailable",
    )?;
    let value = py_strip(&response.stdout)
        .parse::<i64>()
        .map_err(|_| WorkflowError::msg("Invalid NVIDIA VRAM observation"))?;
    require(value > 0, "Invalid NVIDIA VRAM observation")?;
    Ok(value)
}
