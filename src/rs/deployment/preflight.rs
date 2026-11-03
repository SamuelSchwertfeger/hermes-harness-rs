//! Read-only deployment readiness; never modifies gateway or host settings.
use crate::util::PathLexical as _;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::gpu::{CommandRunner, GPUPolicy, ProbeRunner, gpu_lane};
use super::providers::{
    ProviderError, ProviderProbe, ProviderRegistry, ProviderSpec, probe_provider,
};
use crate::multiagent::runtime::WorkflowError;
use crate::util::ordered_map::OrderedMap;

/// One readiness check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// PASS, WARN, or BLOCK.
    pub status: &'static str,
    /// Operator-facing detail.
    pub detail: String,
}
/// Read-only report in Python field order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[allow(clippy::struct_excessive_bools)] // mirrors the Python report
pub struct PreflightReport {
    /// Report mode.
    pub mode: &'static str,
    /// Checks in evaluation order.
    pub checks: OrderedMap<Check>,
    /// Every mandatory check passed.
    pub ready: bool,
    /// Always false.
    pub host_mutated: bool,
    /// Always false.
    pub model_switched: bool,
    /// Always false.
    pub gateway_modified: bool,
    /// Always false.
    pub generated_code_executed: bool,
}
/// Host observations separated from report policy for deterministic offline tests.
pub trait PreflightHost {
    /// Python runtime qualification.
    fn python_314(&self) -> bool;
    /// Executable is on the service PATH.
    fn which(&self, name: &str) -> bool;
    /// Workspace allows read and write access.
    fn workspace_access(&self, workspace: &Path) -> bool;
    /// Dedicated non-root identity.
    fn dedicated_identity(&self) -> bool;
    /// Read-only gateway state.
    /// # Errors
    /// Returns gateway observation failures.
    fn gateway_active(&self, unit: &str) -> Result<bool, WorkflowError>;
    /// Explicitly requested endpoint probe.
    /// # Errors
    /// Returns provider qualification failures.
    fn probe(&self, spec: &ProviderSpec) -> Result<ProviderProbe, ProviderError>;
}
/// Real read-only host checks.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalHost;
impl PreflightHost for LocalHost {
    fn python_314(&self) -> bool {
        #[cfg(unix)]
        let argv = &[
            "/usr/bin/python3",
            "-I",
            "-c",
            "import sys; print(int(sys.version_info >= (3, 14)))",
        ][..];
        #[cfg(not(unix))]
        let argv = &[
            "python",
            "-c",
            "import sys; print(int(sys.version_info >= (3, 14)))",
        ][..];
        CommandRunner
            .run(argv, Duration::from_secs(8))
            .is_ok_and(|r| r.returncode == 0 && r.stdout.trim() == "1")
    }
    fn which(&self, name: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|path| {
                executable(&path.join(name))
                    || cfg!(windows) && path.join(format!("{name}.exe")).is_file()
            })
        })
    }
    fn workspace_access(&self, workspace: &Path) -> bool {
        #[cfg(unix)]
        {
            rustix::fs::access(
                workspace,
                rustix::fs::Access::READ_OK | rustix::fs::Access::WRITE_OK,
            )
            .is_ok()
        }
        #[cfg(not(unix))]
        {
            workspace
                .metadata()
                .is_ok_and(|m| !m.permissions().readonly())
        }
    }
    fn dedicated_identity(&self) -> bool {
        #[cfg(unix)]
        {
            std::env::var_os("HOME").is_some_and(|p| Path::new(&p) != Path::new("/home/operator"))
                && !rustix::process::geteuid().is_root()
        }
        #[cfg(not(unix))]
        {
            std::env::var_os("USERPROFILE")
                .is_some_and(|p| Path::new(&p) != Path::new("/home/operator"))
        }
    }
    fn gateway_active(&self, unit: &str) -> Result<bool, WorkflowError> {
        let response = CommandRunner.run(
            &["/usr/bin/systemctl", "show", unit, "--property=ActiveState"],
            Duration::from_secs(8),
        )?;
        Ok(response.returncode == 0 && response.stdout.contains("ActiveState=active"))
    }
    fn probe(&self, spec: &ProviderSpec) -> Result<ProviderProbe, ProviderError> {
        probe_provider(spec)
    }
}
fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        path.is_file() && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}
fn record(checks: &mut OrderedMap<Check>, name: &str, detail: &str, ok: bool, optional: bool) {
    checks.insert(
        name.to_owned(),
        Check {
            status: if ok {
                "PASS"
            } else if optional {
                "WARN"
            } else {
                "BLOCK"
            },
            detail: detail.to_owned(),
        },
    );
}
fn providers(
    checks: &mut OrderedMap<Check>,
    registry: Option<&ProviderRegistry>,
    network: bool,
    host: &dyn PreflightHost,
) {
    let Some(registry) = registry else {
        record(
            checks,
            "provider",
            "No operator registry configured",
            false,
            false,
        );
        return;
    };
    for (role, _) in registry.roles.iter() {
        if let Err(error) = registry.role(role) {
            record(checks, "provider", &error.to_string(), false, false);
            return;
        }
    }
    record(
        checks,
        "provider",
        "Role profiles validated (not yet live qualified)",
        true,
        false,
    );
    if network {
        for (name, spec) in registry.profiles.iter() {
            let key = format!("endpoint:{name}");
            match host.probe(spec) {
                Ok(result) => record(
                    checks,
                    &key,
                    "Authenticated identity probe passed; 64K/tool reliability unverified",
                    result.model_identity_verified,
                    false,
                ),
                Err(error) => record(checks, &key, &error.to_string(), false, false),
            }
        }
    }
}
/// Probe readiness without host mutation; network requires opt-in.
/// # Errors
/// Propagates failures to run the explicitly supplied gateway probe.
pub fn hp_preflight(
    workspace: &Path,
    registry: Option<&ProviderRegistry>,
    gpu: Option<&GPUPolicy>,
    probe_network: bool,
    gateway_unit: &str,
) -> Result<PreflightReport, WorkflowError> {
    hp_preflight_with(
        workspace,
        registry,
        gpu,
        probe_network,
        gateway_unit,
        &LocalHost,
    )
}
/// Run preflight with caller-supplied host observations.
/// # Errors
/// Propagates gateway observation errors.
pub fn hp_preflight_with(
    workspace: &Path,
    registry: Option<&ProviderRegistry>,
    gpu: Option<&GPUPolicy>,
    probe_network: bool,
    gateway_unit: &str,
    host: &dyn PreflightHost,
) -> Result<PreflightReport, WorkflowError> {
    let mut checks = host_checks(workspace, host);
    gateway(&mut checks, gateway_unit, host)?;
    providers(&mut checks, registry, probe_network, host);
    gpu_check(&mut checks, gpu);
    record(
        &mut checks,
        "native_session",
        "Real Hermes gateway skill receipt not supplied",
        false,
        false,
    );
    record(
        &mut checks,
        "context_64k",
        "Long-prompt/token-usage qualification not supplied",
        false,
        false,
    );
    record(
        &mut checks,
        "tool_qualification",
        "Actual tool-execution receipts not supplied",
        false,
        false,
    );
    let ready = checks.iter().all(|(_, check)| check.status != "BLOCK");
    Ok(PreflightReport {
        mode: "readonly-preflight",
        checks,
        ready,
        host_mutated: false,
        model_switched: false,
        gateway_modified: false,
        generated_code_executed: false,
    })
}
fn host_checks(workspace: &Path, host: &dyn PreflightHost) -> OrderedMap<Check> {
    let mut checks = OrderedMap::new();
    record(
        &mut checks,
        "python",
        "Python >=3.14 required",
        host.python_314(),
        false,
    );
    record(
        &mut checks,
        "uv",
        "uv must be on service PATH",
        host.which("uv"),
        false,
    );
    record(
        &mut checks,
        "bubblewrap",
        "Bubblewrap required for functional tests",
        host.which("bwrap"),
        false,
    );
    record(
        &mut checks,
        "workspace",
        "Existing hermes-owned disposable workspace required",
        workspace.is_dir() && !workspace.is_symlink_lexical() && host.workspace_access(workspace),
        false,
    );
    record(
        &mut checks,
        "identity",
        "Run as dedicated non-root Hermes identity",
        host.dedicated_identity(),
        false,
    );
    checks
}
fn gateway(
    checks: &mut OrderedMap<Check>,
    gateway_unit: &str,
    host: &dyn PreflightHost,
) -> Result<(), WorkflowError> {
    if gateway_unit.is_empty() {
        record(checks, "gateway", "Gateway unit not supplied", false, true);
    } else if !gateway_unit.ends_with(".service") || gateway_unit.contains('/') {
        record(checks, "gateway", "Invalid gateway unit name", false, false);
    } else {
        record(
            checks,
            "gateway",
            "Existing gateway active state (no restart)",
            host.gateway_active(gateway_unit)?,
            false,
        );
    }
    Ok(())
}
fn gpu_check(checks: &mut OrderedMap<Check>, gpu: Option<&GPUPolicy>) {
    if let Some(gpu) = gpu {
        let result = gpu_lane(gpu).and_then(|_lane| gpu.admit(&[]));
        match result {
            Ok(()) => record(
                checks,
                "gpu_lane",
                "Lease acquired and pinned estimate admitted",
                true,
                false,
            ),
            Err(error) => record(
                checks,
                "gpu_lane",
                if matches!(error, WorkflowError::Io(_)) {
                    "OSError"
                } else {
                    "WorkflowError"
                },
                false,
                false,
            ),
        }
    } else {
        record(checks, "gpu_lane", "GPU policy missing", false, true);
    }
}
