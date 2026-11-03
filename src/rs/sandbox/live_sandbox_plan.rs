//! Render bounded systemd/Bubblewrap plans; never execute privileged commands.
//!
//! These plans are operator-reviewable launch specifications, not runtime qualification
//! or permission to dispatch live Hermes gateway requests.

use crate::util::PathLexical as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

crate::util::wire_enum! {
    /// Which side of the worker/reviewer split a job runs on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub enum Role {
        /// May write to the workspace mount.
        Worker => "worker",
        /// Read-only everywhere.
        Reviewer => "reviewer",
    }

    /// The Python string value of the role.
    as_str;
}

/// An unsafe or incomplete sandbox plan was rejected.
#[derive(Debug, thiserror::Error)]
pub enum SandboxPlanError {
    /// A validation rule was violated.
    #[error("{0}")]
    Invalid(&'static str),
    /// A path could not be resolved.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Operator-chosen paths, fixed resource policy and role identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxInputs {
    /// Role identity.
    pub role: Role,
    /// systemd unit name (must use the `hermes-job-` namespace).
    pub unit: String,
    /// Workspace mount source.
    pub workspace: PathBuf,
    /// Evidence mount source.
    pub evidence: PathBuf,
    /// Executable under `/usr`.
    pub executable: PathBuf,
    /// Explicit argument vector for the executable.
    pub argv: Vec<String>,
    /// Memory limit; only `512M` is accepted.
    pub memory_max: String,
    /// CPU quota; only `50%` is accepted.
    pub cpu_quota: String,
    /// Task limit; only `32` is accepted.
    pub tasks_max: i64,
}

impl SandboxInputs {
    /// Inputs with the fixed default resource policy.
    #[must_use]
    pub fn new(
        role: Role,
        unit: &str,
        workspace: &Path,
        evidence: &Path,
        executable: &Path,
        argv: &[&str],
    ) -> Self {
        Self {
            role,
            unit: unit.to_owned(),
            workspace: workspace.to_path_buf(),
            evidence: evidence.to_path_buf(),
            executable: executable.to_path_buf(),
            argv: argv.iter().map(|value| (*value).to_owned()).collect(),
            memory_max: "512M".to_owned(),
            cpu_quota: "50%".to_owned(),
            tasks_max: 32,
        }
    }
}

/// Immutable structured plan, suitable for inspecting before manual execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPlan {
    /// Role identity.
    pub role: Role,
    /// systemd unit name.
    pub unit: String,
    /// Full `systemd-run` launch vector.
    pub systemd_argv: Vec<String>,
    /// Exact stop vector.
    pub stop_argv: Vec<String>,
}

/// Python `json.dumps` string escaping with `ensure_ascii=True`.
/// Key order of the Python dict literal in `as_json`.
#[derive(Serialize)]
struct PlanJson<'a> {
    role: &'a str,
    unit: &'a str,
    systemd_argv: &'a [String],
    stop_argv: &'a [String],
}

impl SandboxPlan {
    /// Serialize argv without shell interpretation or execution.
    ///
    /// Equals Python `json.dumps({...}, indent=2)`: insertion-ordered keys, ASCII-escaped.
    #[must_use]
    pub fn to_json(&self) -> String {
        let plan = PlanJson {
            role: self.role.as_str(),
            unit: &self.unit,
            systemd_argv: &self.systemd_argv,
            stop_argv: &self.stop_argv,
        };
        // Only strings and string arrays: encoding cannot fail.
        crate::util::pretty_json_ordered(&plan).unwrap_or_default()
    }
}

fn unit_is_valid(unit: &str) -> bool {
    let bytes = unit.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .skip(1)
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

fn directory(path: &Path) -> Result<PathBuf, SandboxPlanError> {
    if path.is_symlink_lexical() || !path.is_dir() {
        return Err(SandboxPlanError::Invalid(
            "Mount source must be a real existing directory",
        ));
    }
    let resolved = path.canonicalize()?;
    if resolved == Path::new("/") || resolved == Path::new("/home/hermes") {
        return Err(SandboxPlanError::Invalid(
            "Host root and Hermes home must never be mounted",
        ));
    }
    Ok(resolved)
}

/// Validate all operator-chosen data before constructing a launch command.
///
/// # Errors
/// Sandbox paths, unit, executable, arguments, or resource policy are invalid.
pub fn validate_sandbox_inputs(
    inputs: &SandboxInputs,
) -> Result<(PathBuf, PathBuf, PathBuf), SandboxPlanError> {
    if !unit_is_valid(&inputs.unit) || !inputs.unit.starts_with("hermes-job-") {
        return Err(SandboxPlanError::Invalid(
            "Unit must use the hermes-job- namespace",
        ));
    }
    let workspace = directory(&inputs.workspace)?;
    let evidence = directory(&inputs.evidence)?;
    if workspace == evidence
        || evidence.ancestors().skip(1).any(|p| p == workspace)
        || workspace.ancestors().skip(1).any(|p| p == evidence)
    {
        return Err(SandboxPlanError::Invalid(
            "Workspace and evidence mounts must be disjoint",
        ));
    }
    if inputs.executable.is_symlink_lexical() || !inputs.executable.is_file() {
        return Err(SandboxPlanError::Invalid(
            "Executable must be a real existing file",
        ));
    }
    let executable = inputs.executable.canonicalize()?;
    if !executable.starts_with("/usr") {
        return Err(SandboxPlanError::Invalid(
            "Only system-installed executables under /usr are supported",
        ));
    }
    if inputs.argv.is_empty()
        || inputs
            .argv
            .iter()
            .any(|value| value.is_empty() || value.contains('\0'))
    {
        return Err(SandboxPlanError::Invalid(
            "Explicit nonempty argv is required",
        ));
    }
    if inputs.memory_max != "512M" || inputs.cpu_quota != "50%" || inputs.tasks_max != 32 {
        return Err(SandboxPlanError::Invalid(
            "Resource policy is fixed until host-qualified alternatives exist",
        ));
    }
    Ok((workspace, evidence, executable))
}

/// Build an isolated job plan; refuse permissive or ambiguous configuration.
///
/// # Errors
/// Sandbox paths, unit, executable, arguments, or resource policy are invalid.
pub fn build_sandbox_plan(inputs: &SandboxInputs) -> Result<SandboxPlan, SandboxPlanError> {
    let (workspace, evidence, executable) = validate_sandbox_inputs(inputs)?;
    let role_mount = match inputs.role {
        Role::Worker => "--bind",
        Role::Reviewer => "--ro-bind",
    };
    let unit_flag = format!("--unit={}", inputs.unit);
    let mut command: Vec<String> = [
        "/usr/bin/systemd-run",
        &unit_flag,
        "--uid=hermes",
        "--property=MemoryMax=512M",
        "--property=CPUQuota=50%",
        "--property=TasksMax=32",
        "--property=KillMode=control-group",
        "--property=TimeoutStopSec=5s",
        "--property=NoNewPrivileges=yes",
        "--property=PrivateDevices=yes",
        "--property=ProtectSystem=strict",
        "/usr/bin/bwrap",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--die-with-parent",
        "--new-session",
        "--ro-bind",
        "/usr",
        "/usr",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/workspace",
        "--dir",
        "/evidence",
        role_mount,
        crate::util::path_str(&workspace)?,
        "/workspace",
        "--ro-bind",
        crate::util::path_str(&evidence)?,
        "/evidence",
        "--chdir",
        "/workspace",
        crate::util::path_str(&executable)?,
    ]
    .iter()
    .map(|value| (*value).to_owned())
    .collect();
    command.extend(inputs.argv.iter().cloned());
    Ok(SandboxPlan {
        role: inputs.role,
        unit: inputs.unit.clone(),
        systemd_argv: command,
        stop_argv: vec![
            "/usr/bin/systemctl".to_owned(),
            "stop".to_owned(),
            format!("{}.service", inputs.unit),
        ],
    })
}
