//! Internal deployment bridge; only explicit GPU switching changes host services.
use std::path::{Path, PathBuf};

use clap::{Args, Command, Subcommand};
use serde::Serialize;

use super::evaluation::{evaluate_samples, load_samples};
use super::gpu::{
    CommandRunner, ProbeRunner, SwitchRequest, Systemd, UserSystemd, load_gpu_policy,
    observed_free_vram_mib, switch_gpu_slot, write_once,
};
use super::preflight::{LocalHost, PreflightHost, hp_preflight_with};
use super::providers::load_registry;
use crate::multiagent::ollama_native::OrderedJson;
use crate::multiagent::runtime::WorkflowError;
use crate::util::ordered_map::OrderedMap;
use crate::util::{ordered_json_spaced, py_path_str, resolve_lenient};

/// Internal bridge commands, never an implicit authorization.
#[derive(Debug, Clone, Subcommand)]
pub enum DeploymentCommand {
    /// Read-only host qualification.
    DeployPreflight {
        /// Disposable workspace.
        #[arg(long)]
        workspace: PathBuf,
        /// Optional operator registry.
        #[arg(long)]
        provider_registry: Option<PathBuf>,
        /// Optional GPU policy.
        #[arg(long)]
        gpu_policy: Option<PathBuf>,
        /// Existing gateway unit.
        #[arg(long, default_value = "")]
        gateway_unit: String,
        /// Explicit network opt-in.
        #[arg(long)]
        probe_network: bool,
        /// Session binding.
        #[arg(long)]
        session: String,
    },
    /// Probe one explicitly configured provider role.
    ProviderProbe {
        /// Registry path.
        #[arg(long)]
        provider_registry: PathBuf,
        /// Approved role.
        #[arg(long)]
        role: String,
        /// Session binding.
        #[arg(long)]
        session: String,
    },
    /// Analyze offline receipts.
    ModelEvaluate {
        /// Sample document.
        #[arg(long)]
        sample_file: PathBuf,
        /// Existing evidence directory.
        #[arg(long)]
        evidence_root: PathBuf,
        /// Optional write-once output.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Separate recording authorization.
        #[arg(long)]
        authorize_record: bool,
        /// Session binding.
        #[arg(long)]
        session: String,
    },
    /// One authorized host service switch.
    GpuSwitch(SwitchArgs),
}
/// Explicit GPU switch arguments.
#[derive(Debug, Clone, Args)]
pub struct SwitchArgs {
    /// Operator policy.
    #[arg(long)]
    pub gpu_policy: PathBuf,
    /// Operator registry.
    #[arg(long)]
    pub provider_registry: PathBuf,
    /// Previously active unit.
    #[arg(long)]
    pub prior_unit: String,
    /// Desired unit.
    #[arg(long)]
    pub target_unit: String,
    /// One-shot identity.
    #[arg(long)]
    pub transaction: String,
    /// Journal directory.
    #[arg(long)]
    pub journal: PathBuf,
    /// Separate host-change authorization.
    #[arg(long)]
    pub authorize_switch: bool,
    /// Session binding.
    #[arg(long)]
    pub session: String,
}
/// Register opt-in bridge commands in a caller-owned parser.
#[must_use]
pub fn add_deployment_commands(command: Command) -> Command {
    DeploymentCommand::augment_subcommands(command)
}
fn ordered(value: &impl Serialize) -> Result<OrderedMap<OrderedJson>, WorkflowError> {
    Ok(serde_json::from_str(&ordered_json_spaced(value)?)?)
}
fn with_session(session: &str, result: &OrderedMap<OrderedJson>) -> OrderedMap<OrderedJson> {
    let mut report = OrderedMap::new();
    report.insert("session_id".to_owned(), OrderedJson::from(session));
    for (name, value) in result.iter() {
        report.insert(name.clone(), value.clone());
    }
    report
}
fn evaluate(
    path: &Path,
    root: &Path,
    output: Option<&Path>,
    authorized: bool,
) -> Result<OrderedMap<OrderedJson>, WorkflowError> {
    let report = evaluate_samples(&load_samples(path, root)?)?;
    let mut result = ordered(&report)?;
    if let Some(output) = output {
        if !authorized {
            return Err(WorkflowError::msg(
                "Evaluation recording requires separate authorization",
            ));
        }
        if output
            .parent()
            .map(|p| {
                if p.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    p
                }
            })
            .map(resolve_lenient)
            != Some(resolve_lenient(root))
        {
            return Err(WorkflowError::msg(
                "Evaluation output must remain inside evidence root",
            ));
        }
        let digest = write_once(output, &report)?;
        result.insert(
            "evidence".to_owned(),
            OrderedJson::from(py_path_str(output)),
        );
        result.insert("evidence_sha256".to_owned(), OrderedJson::from(digest));
    }
    Ok(result)
}
fn switch(
    args: &SwitchArgs,
    host: &dyn PreflightHost,
    systemd: &mut dyn Systemd,
    hostname: &str,
    free: &mut dyn FnMut() -> Result<i64, WorkflowError>,
) -> Result<OrderedMap<OrderedJson>, WorkflowError> {
    let policy = load_gpu_policy(&args.gpu_policy)?;
    if hostname != policy.host {
        return Err(WorkflowError::msg(
            "GPU service switching only permitted on owning host",
        ));
    }
    let registry =
        load_registry(&args.provider_registry).map_err(|e| WorkflowError::msg(&e.to_string()))?;
    let matched: Vec<_> = registry
        .profiles
        .iter()
        .map(|(_, s)| s)
        .filter(|s| s.unit == args.target_unit && s.gpu_host == policy.host)
        .collect();
    let Some(spec) = matched.first().filter(|_| matched.len() == 1) else {
        return Err(WorkflowError::msg(
            "GPU target has no unique registered provider identity",
        ));
    };
    // Deviation: provider/inference failures from ready also write UNKNOWN in Rust;
    // Python's exception tuple omits ProviderError. Preserve fail-closed reconciliation.
    let result = switch_gpu_slot(
        SwitchRequest {
            policy: &policy,
            prior: &args.prior_unit,
            target: &args.target_unit,
            transaction: &args.transaction,
            journal: &args.journal,
            authorized: args.authorize_switch,
        },
        systemd,
        &mut |unit| {
            Ok(unit.model == spec.model
                && host
                    .probe(spec)
                    .map_err(|e| WorkflowError::msg(&e.to_string()))?
                    .model_identity_verified)
        },
        Some(free),
    )?;
    ordered(&result)
}
/// Dispatch an explicit deployment command.
/// # Errors
/// Propagates policy, authorization, evidence and probe failures.
pub fn handle_deployment(
    args: &DeploymentCommand,
) -> Result<OrderedMap<OrderedJson>, WorkflowError> {
    let hostname = if matches!(args, DeploymentCommand::GpuSwitch(_)) {
        CommandRunner.hostname()?
    } else {
        String::new()
    };
    handle_deployment_with(
        args,
        &LocalHost,
        &mut UserSystemd::<CommandRunner>::default(),
        &hostname,
        &mut observed_free_vram_mib,
    )
}
/// Handle deployment with caller-supplied host observations and service operations.
/// # Errors
/// Propagates policy, authorization, evidence and probe failures.
pub fn handle_deployment_with(
    args: &DeploymentCommand,
    host: &dyn PreflightHost,
    systemd: &mut dyn Systemd,
    hostname: &str,
    free: &mut dyn FnMut() -> Result<i64, WorkflowError>,
) -> Result<OrderedMap<OrderedJson>, WorkflowError> {
    let (session, result) = match args {
        DeploymentCommand::DeployPreflight {
            workspace,
            provider_registry,
            gpu_policy,
            gateway_unit,
            probe_network,
            session,
        } => {
            let registry = provider_registry
                .as_deref()
                .map(load_registry)
                .transpose()
                .map_err(|e| WorkflowError::msg(&e.to_string()))?;
            let policy = gpu_policy.as_deref().map(load_gpu_policy).transpose()?;
            (
                session,
                ordered(&hp_preflight_with(
                    workspace,
                    registry.as_ref(),
                    policy.as_ref(),
                    *probe_network,
                    gateway_unit,
                    host,
                )?)?,
            )
        }
        DeploymentCommand::ProviderProbe {
            provider_registry,
            role,
            session,
        } => {
            let registry =
                load_registry(provider_registry).map_err(|e| WorkflowError::msg(&e.to_string()))?;
            let spec = registry
                .role(role)
                .map_err(|e| WorkflowError::msg(&e.to_string()))?;
            (
                session,
                ordered(
                    &host
                        .probe(spec)
                        .map_err(|e| WorkflowError::msg(&e.to_string()))?,
                )?,
            )
        }
        DeploymentCommand::ModelEvaluate {
            sample_file,
            evidence_root,
            output,
            authorize_record,
            session,
        } => (
            session,
            evaluate(
                sample_file,
                evidence_root,
                output.as_deref(),
                *authorize_record,
            )?,
        ),
        DeploymentCommand::GpuSwitch(args) => {
            (&args.session, switch(args, host, systemd, hostname, free)?)
        }
    };
    Ok(with_session(session, &result))
}
