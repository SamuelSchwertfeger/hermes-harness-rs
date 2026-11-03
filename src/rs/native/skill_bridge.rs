//! Bounded JSON command bridge invoked through Hermes's existing terminal tool.
//!
//! This is NOT a new Hermes slash-command router. Skills call this diagnostic, read-mostly
//! backend entrypoint from the originating Hermes session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::{Map, Value, json};

use super::functional_verify::functional_verify;
use super::lifecycle::record_checkpoint;
use super::live_assembly::{LiveAssembly, LiveAssemblyError, apply_live_assembly, live_assemble};
use super::live_review::{LiveReview, reviewed_live_task};
use super::repair_controller::{
    DispositionRequest, inspect_repair, open_repair, record_repair_disposition, reserve_repair,
};
use super::repair_dispatch::{
    DispatchRequest, RepairDispatchError, dispatch_repair, inspect_dispatch,
};
use super::stage_approvals::{approve, begin, snapshot, submit};
use super::verify_applied::verify_applied;
use super::workflows::{NativeError, repository};
use super::worktree_apply::{WorktreeApplyError, apply_reviewed_receipt};
use crate::deployment::bridge::{DeploymentCommand, handle_deployment};
use crate::multiagent::dynamic_pool::{XPS_BASE, available_models, infer};
use crate::multiagent::local_inference::LocalInferenceError;
use crate::multiagent::plan_io::parse_plan_json;
use crate::multiagent::runtime::{WorkflowError, inspect, ordered_nodes};
use crate::multiagent::sandbox_tools::{record_sandbox_tool, run_sandbox_tool};
use crate::util::{
    JsonError, PathLexical as _, PyStr as _, py_path_str, sorted_json_spaced_fields,
    sorted_json_spaced_value,
};

/// Largest accepted plan file.
pub const MAX_FILE_SIZE: u64 = 65536;
/// Longest accepted session identifier, in characters.
pub const MAX_SESSION_ID_LENGTH: usize = 128;

/// Failure of any bridge operation.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// Workflow rule violation or evidence failure.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Native workflow failure.
    #[error(transparent)]
    Native(#[from] NativeError),
    /// Live assembly failure.
    #[error(transparent)]
    LiveAssembly(#[from] LiveAssemblyError),
    /// Worktree application failure.
    #[error(transparent)]
    Apply(#[from] WorktreeApplyError),
    /// Repair dispatch failure.
    #[error(transparent)]
    RepairDispatch(#[from] RepairDispatchError),
    /// Local inference failure.
    #[error(transparent)]
    Inference(#[from] LocalInferenceError),
    /// Filesystem failure.
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// Malformed JSON.
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

fn fail<T>(message: &str) -> Result<T, BridgeError> {
    Err(WorkflowError::msg(message).into())
}

/// Workflow stage choices (Python `choices=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum StageArg {
    /// `lets-investigate`
    LetsInvestigate,
    /// `what-we-know`
    WhatWeKnow,
    /// `agents-assemble`
    AgentsAssemble,
    /// `stick-the-landing`
    StickTheLanding,
}

impl StageArg {
    const fn as_str(self) -> &'static str {
        match self {
            Self::LetsInvestigate => "lets-investigate",
            Self::WhatWeKnow => "what-we-know",
            Self::AgentsAssemble => "agents-assemble",
            Self::StickTheLanding => "stick-the-landing",
        }
    }
}

/// Repair outcome choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutcomeArg {
    /// `rejected`
    Rejected,
    /// `failed`
    Failed,
    /// `passed`
    Passed,
    /// `abandoned`
    Abandoned,
}

impl OutcomeArg {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Passed => "passed",
            Self::Abandoned => "abandoned",
        }
    }
}

/// Read-only sandbox tool choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ToolArg {
    /// `list-files`
    ListFiles,
    /// `search-fixed`
    SearchFixed,
    /// `git-status`
    GitStatus,
}

impl ToolArg {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ListFiles => "list-files",
            Self::SearchFixed => "search-fixed",
            Self::GitStatus => "git-status",
        }
    }
}

/// Bridge subcommands (Python `argparse` sub-parsers).
#[derive(Debug, Clone, Subcommand)]
pub enum BridgeCommand {
    /// Opt-in deployment commands.
    #[command(flatten)]
    Deployment(DeploymentCommand),
    /// Report capability facts.
    Doctor,
    /// List XPS models.
    XpsModels {
        /// Hermes session.
        #[arg(long)]
        session: String,
    },
    /// Run one authorized XPS inference.
    XpsInfer {
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Model name.
        #[arg(long)]
        model: String,
        /// Prompt.
        #[arg(long)]
        prompt: String,
        /// Explicit inference grant.
        #[arg(long)]
        authorize_inference: bool,
    },
    /// Begin a staged workflow.
    WorkflowBegin {
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Run identifier.
        #[arg(long)]
        run_id: String,
        /// Repository.
        #[arg(long)]
        repo: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
    },
    /// Submit stage evidence.
    WorkflowSubmit {
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Run identifier.
        #[arg(long)]
        run_id: String,
        /// Stage.
        #[arg(long)]
        stage: StageArg,
        /// Evidence file.
        #[arg(long)]
        evidence: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
    },
    /// Approve a stage.
    WorkflowApprove {
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Run identifier.
        #[arg(long)]
        run_id: String,
        /// Stage.
        #[arg(long)]
        stage: StageArg,
        /// Evidence digest.
        #[arg(long)]
        evidence_sha256: String,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Operator confirmation.
        #[arg(long)]
        operator_confirmed: bool,
    },
    /// Show workflow status.
    WorkflowStatus {
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Run identifier.
        #[arg(long)]
        run_id: String,
        /// Hermes session.
        #[arg(long)]
        session: String,
    },
    /// Run one read-only sandbox tool.
    SandboxTool {
        /// Repository.
        #[arg(long)]
        repo: PathBuf,
        /// Tool.
        #[arg(long)]
        tool: ToolArg,
        /// Search pattern.
        #[arg(long, default_value = "")]
        pattern: String,
        /// Evidence file.
        #[arg(long)]
        evidence: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit execution grant.
        #[arg(long)]
        authorize_execution: bool,
    },
    /// Validate a plan file.
    Plan {
        /// Plan file.
        #[arg(long)]
        file: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
    },
    /// Assemble live tasks.
    AssembleLive {
        /// Repository.
        #[arg(long)]
        repo: PathBuf,
        /// Plan file.
        #[arg(long)]
        plan: PathBuf,
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Provider registry.
        #[arg(long)]
        provider_registry: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit inference grant.
        #[arg(long)]
        authorize_inference: bool,
        /// Explicit tool grant.
        #[arg(long)]
        authorize_tools: bool,
        /// XPS worker model.
        #[arg(long)]
        xps_worker_model: Option<String>,
        /// XPS reviewer model.
        #[arg(long)]
        xps_reviewer_model: Option<String>,
    },
    /// Apply a live assembly receipt.
    ApplyLiveAssembly {
        /// Repository.
        #[arg(long)]
        repo: PathBuf,
        /// Receipt.
        #[arg(long)]
        receipt: PathBuf,
        /// Workspace.
        #[arg(long)]
        workspace: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit application grant.
        #[arg(long)]
        authorize_apply: bool,
    },
    /// Propose and review one live task.
    LiveReview {
        /// Repository.
        #[arg(long)]
        repo: PathBuf,
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Task identifier.
        #[arg(long)]
        task_id: String,
        /// Objective.
        #[arg(long)]
        objective: String,
        /// Owned path.
        #[arg(long)]
        owned_path: String,
        /// Explicit inference grant.
        #[arg(long)]
        authorize_inference: bool,
    },
    /// Apply a reviewed receipt to a disposable worktree.
    ApplyReviewed {
        /// Repository.
        #[arg(long)]
        repo: PathBuf,
        /// Receipt.
        #[arg(long)]
        receipt: PathBuf,
        /// Workspace.
        #[arg(long)]
        workspace: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit application grant.
        #[arg(long)]
        authorize_apply: bool,
    },
    /// Record verification of an applied worktree.
    VerifyApplied {
        /// Repository.
        #[arg(long)]
        repo: PathBuf,
        /// Receipt.
        #[arg(long)]
        receipt: PathBuf,
        /// Workspace.
        #[arg(long)]
        workspace: PathBuf,
        /// Evidence file.
        #[arg(long)]
        evidence: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit verification grant.
        #[arg(long)]
        authorize_verify: bool,
    },
    /// Record an integrity checkpoint.
    Checkpoint {
        /// Receipt.
        #[arg(long)]
        receipt: PathBuf,
        /// Verification receipt.
        #[arg(long)]
        verification: PathBuf,
        /// Evidence file.
        #[arg(long)]
        evidence: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit record grant.
        #[arg(long)]
        authorize_record: bool,
    },
    /// Run sandboxed functional verification.
    FunctionalVerify {
        /// Checkpoint.
        #[arg(long)]
        checkpoint: PathBuf,
        /// Workspace.
        #[arg(long)]
        workspace: PathBuf,
        /// Evidence file.
        #[arg(long)]
        evidence: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit execution grant.
        #[arg(long)]
        authorize_execution: bool,
    },
    /// Open repair tracking.
    RepairOpen {
        /// Failure receipt.
        #[arg(long)]
        failure: PathBuf,
        /// Checkpoint.
        #[arg(long)]
        checkpoint: PathBuf,
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit record grant.
        #[arg(long)]
        authorize_record: bool,
    },
    /// Reserve a repair attempt.
    RepairReserve {
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Task identifier.
        #[arg(long)]
        task_id: String,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit reserve grant.
        #[arg(long)]
        authorize_reserve: bool,
    },
    /// Record a repair attempt outcome.
    RepairDisposition {
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Task identifier.
        #[arg(long)]
        task_id: String,
        /// Outcome.
        #[arg(long)]
        outcome: OutcomeArg,
        /// Review receipt.
        #[arg(long)]
        review: Option<PathBuf>,
        /// Verification receipt.
        #[arg(long)]
        verification: Option<PathBuf>,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit record grant.
        #[arg(long)]
        authorize_record: bool,
    },
    /// Dispatch a repair to local models.
    RepairDispatch {
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Task identifier.
        #[arg(long)]
        task_id: String,
        /// Checkpoint.
        #[arg(long)]
        checkpoint: PathBuf,
        /// Objective.
        #[arg(long)]
        objective: String,
        /// Owned paths.
        #[arg(long, required = true)]
        owned_path: Vec<String>,
        /// Hermes session.
        #[arg(long)]
        session: String,
        /// Explicit inference grant.
        #[arg(long)]
        authorize_inference: bool,
        /// Provider registry.
        #[arg(long)]
        provider_registry: Option<PathBuf>,
        /// GPU policy.
        #[arg(long)]
        gpu_policy: Option<PathBuf>,
    },
    /// Inspect repair state.
    RepairInspect {
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Task identifier.
        #[arg(long)]
        task_id: String,
        /// Hermes session.
        #[arg(long)]
        session: String,
    },
    /// Inspect a persisted run.
    Inspect {
        /// State root.
        #[arg(long)]
        state: PathBuf,
        /// Run identifier.
        #[arg(long)]
        run_id: String,
        /// Hermes session.
        #[arg(long)]
        session: String,
    },
}

/// Command line of the Hermes skill bridge.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "hermes-harness-skill-bridge",
    args_override_self = true,
    infer_long_args = true
)]
pub struct Args {
    /// Bridge operation.
    #[command(subcommand)]
    pub command: BridgeCommand,
}

fn session_of(command: &BridgeCommand) -> Option<&str> {
    use BridgeCommand as C;
    use DeploymentCommand as D;
    match command {
        C::Doctor => None,
        C::Deployment(
            D::DeployPreflight { session, .. }
            | D::ProviderProbe { session, .. }
            | D::ModelEvaluate { session, .. },
        )
        | C::XpsModels { session }
        | C::XpsInfer { session, .. }
        | C::WorkflowBegin { session, .. }
        | C::WorkflowSubmit { session, .. }
        | C::WorkflowApprove { session, .. }
        | C::WorkflowStatus { session, .. }
        | C::SandboxTool { session, .. }
        | C::Plan { session, .. }
        | C::AssembleLive { session, .. }
        | C::ApplyLiveAssembly { session, .. }
        | C::LiveReview { session, .. }
        | C::ApplyReviewed { session, .. }
        | C::VerifyApplied { session, .. }
        | C::Checkpoint { session, .. }
        | C::FunctionalVerify { session, .. }
        | C::RepairOpen { session, .. }
        | C::RepairReserve { session, .. }
        | C::RepairDisposition { session, .. }
        | C::RepairDispatch { session, .. }
        | C::RepairInspect { session, .. }
        | C::Inspect { session, .. } => Some(session),
        C::Deployment(D::GpuSwitch(switch)) => Some(&switch.session),
    }
}

fn check_session(value: &str) -> Result<&str, BridgeError> {
    if value.py_strip().is_empty()
        || value.chars().count() > MAX_SESSION_ID_LENGTH
        || value.contains('\0')
    {
        return fail("Invalid Hermes session identifier");
    }
    Ok(value)
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("result".to_owned(), other);
            map
        }
    }
}

/// `{'session_id': session, **result}`.
fn with_session(session: &str, result: Value) -> Value {
    let mut report = Map::new();
    report.insert("session_id".to_owned(), json!(session));
    report.extend(object(result));
    Value::Object(report)
}

fn doctor() -> Value {
    json!({
        "native_skills": "instruction-only",
        "plan_validation": true,
        "durable_inspection": true,
        "local_model_adapters": "opt-in-unqualified",
        "live_execution_qualified": false,
        "tool_sandbox_qualified": false,
        "human_grant_required": true,
    })
}

fn plan_report(file: &Path, session: &str) -> Result<Value, BridgeError> {
    let size = file.metadata().map(|meta| meta.len());
    if file.is_symlink_lexical() || !file.is_file() || size.is_ok_and(|bytes| bytes > MAX_FILE_SIZE)
    {
        return fail("Missing, symlinked or oversized plan file");
    }
    let plan = parse_plan_json(&std::fs::read_to_string(file)?)?;
    let roles: Map<String, Value> = plan
        .nodes
        .iter()
        .map(|node| (node.id.clone(), json!(node.role.as_str())))
        .collect();
    let ordered: Vec<String> = ordered_nodes(&plan)?
        .into_iter()
        .map(|node| node.id)
        .collect();
    Ok(json!({
        "session_id": session,
        "mode": "validation-only",
        "plan_id": plan.id,
        "ordered_nodes": ordered,
        "roles": roles,
        "dispatched": false,
    }))
}

fn sandbox_report(
    repo: &Path,
    tool: &str,
    pattern: &str,
    evidence: &Path,
    session: &str,
    authorized: bool,
) -> Result<Value, BridgeError> {
    let result = run_sandbox_tool(&repository(repo)?, tool, pattern, authorized)?;
    let digest = record_sandbox_tool(&result, evidence)?;
    Ok(json!({
        "session_id": session,
        "tool": result.tool,
        "exit_code": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "sandbox": result.sandbox,
        "host_execution": result.host_execution,
        "evidence": py_path_str(evidence),
        "evidence_sha256": digest,
    }))
}

fn to_value(item: &impl Serialize) -> Result<Value, BridgeError> {
    Ok(serde_json::to_value(item)?)
}

fn workflow(command: &BridgeCommand, session: &str) -> Result<Option<Value>, BridgeError> {
    Ok(Some(match command {
        BridgeCommand::WorkflowBegin {
            state,
            run_id,
            repo,
            ..
        } => begin(state, run_id, repo, session)?,
        BridgeCommand::WorkflowSubmit {
            state,
            run_id,
            stage,
            evidence,
            ..
        } => submit(state, run_id, stage.as_str(), evidence, session)?,
        BridgeCommand::WorkflowApprove {
            state,
            run_id,
            stage,
            evidence_sha256,
            operator_confirmed,
            ..
        } => approve(
            state,
            run_id,
            stage.as_str(),
            evidence_sha256,
            session,
            *operator_confirmed,
        )?,
        BridgeCommand::WorkflowStatus { state, run_id, .. } => {
            let result = snapshot(state, run_id)?;
            if result.get("session") != Some(&json!(session)) {
                return fail("Originating Hermes session mismatch");
            }
            result
        }
        _ => return Ok(None),
    }))
}

fn repair(command: &BridgeCommand, session: &str) -> Result<Option<Value>, BridgeError> {
    let result = match command {
        BridgeCommand::RepairOpen {
            failure,
            checkpoint,
            state,
            authorize_record,
            ..
        } => open_repair(failure, checkpoint, state, *authorize_record)?,
        BridgeCommand::RepairReserve {
            state,
            task_id,
            authorize_reserve,
            ..
        } => reserve_repair(state, task_id, *authorize_reserve)?,
        BridgeCommand::RepairDisposition {
            state,
            task_id,
            outcome,
            review,
            verification,
            authorize_record,
            ..
        } => record_repair_disposition(&DispositionRequest {
            state,
            task_id,
            outcome: outcome.as_str(),
            review: review.as_deref(),
            verification: verification.as_deref(),
            authorized: *authorize_record,
        })?,
        BridgeCommand::RepairInspect { state, task_id, .. } => {
            let mut result = inspect_repair(state, task_id)?;
            let dispatch = inspect_dispatch(state, task_id)?;
            if let Value::Object(map) = &mut result {
                map.insert("dispatch".to_owned(), dispatch);
            }
            result
        }
        BridgeCommand::RepairDispatch {
            state,
            task_id,
            checkpoint,
            objective,
            owned_path,
            authorize_inference,
            provider_registry,
            gpu_policy,
            ..
        } => dispatch_repair(&DispatchRequest {
            state,
            task_id,
            checkpoint,
            objective,
            owned_paths: owned_path,
            authorized: *authorize_inference,
            worker: None,
            reviewer: None,
            transport: None,
            registry_file: provider_registry.as_deref(),
            gpu_policy_file: gpu_policy.as_deref(),
        })?,
        _ => return Ok(None),
    };
    Ok(Some(with_session(session, result)))
}

/// Receipt application seam, see [`handle_with`].
pub type ApplyReviewed<'a> =
    &'a dyn Fn(&Path, &Path, &Path, bool) -> Result<Value, WorktreeApplyError>;

fn live(command: &BridgeCommand, session: &str) -> Result<Option<Value>, BridgeError> {
    Ok(Some(match command {
        BridgeCommand::AssembleLive {
            repo,
            plan,
            state,
            provider_registry,
            authorize_inference,
            authorize_tools,
            xps_worker_model,
            xps_reviewer_model,
            ..
        } => live_assemble(&LiveAssembly {
            repo,
            plan,
            state,
            registry: provider_registry,
            session,
            authorize_inference: *authorize_inference,
            authorize_tools: *authorize_tools,
            xps_worker_model: xps_worker_model.as_deref(),
            xps_reviewer_model: xps_reviewer_model.as_deref(),
        })?,
        BridgeCommand::ApplyLiveAssembly {
            repo,
            receipt,
            workspace,
            authorize_apply,
            ..
        } => with_session(
            session,
            apply_live_assembly(repo, receipt, workspace, *authorize_apply)?,
        ),
        BridgeCommand::LiveReview {
            repo,
            state,
            task_id,
            objective,
            owned_path,
            authorize_inference,
            ..
        } => reviewed_live_task(&LiveReview {
            repo,
            state,
            session,
            task_id,
            objective,
            owned_path,
            authorized: *authorize_inference,
        })?,
        _ => return Ok(None),
    }))
}

fn evidence_commands(
    command: &BridgeCommand,
    session: &str,
    apply: ApplyReviewed<'_>,
) -> Result<Option<Value>, BridgeError> {
    let result = match command {
        BridgeCommand::ApplyReviewed {
            repo,
            receipt,
            workspace,
            authorize_apply,
            ..
        } => apply(repo, receipt, workspace, *authorize_apply)?,
        BridgeCommand::VerifyApplied {
            repo,
            receipt,
            workspace,
            evidence,
            authorize_verify,
            ..
        } => verify_applied(repo, receipt, workspace, evidence, *authorize_verify)?,
        BridgeCommand::Checkpoint {
            receipt,
            verification,
            evidence,
            authorize_record,
            ..
        } => record_checkpoint(receipt, verification, evidence, *authorize_record)?,
        BridgeCommand::FunctionalVerify {
            checkpoint,
            workspace,
            evidence,
            authorize_execution,
            ..
        } => functional_verify(checkpoint, workspace, evidence, *authorize_execution)?,
        _ => return Ok(None),
    };
    Ok(Some(with_session(session, result)))
}

fn simple(command: &BridgeCommand, session: &str) -> Result<Value, BridgeError> {
    match command {
        BridgeCommand::XpsModels { .. } => Ok(json!({
            "session_id": session,
            "models": available_models(XPS_BASE)?,
            "mode": "read-only",
        })),
        BridgeCommand::XpsInfer {
            model,
            prompt,
            authorize_inference,
            ..
        } => Ok(with_session(
            session,
            to_value(&infer(model, prompt, *authorize_inference)?)?,
        )),
        BridgeCommand::Deployment(deployment) => to_value(&handle_deployment(deployment)?),
        BridgeCommand::SandboxTool {
            repo,
            tool,
            pattern,
            evidence,
            authorize_execution,
            ..
        } => sandbox_report(
            repo,
            tool.as_str(),
            pattern,
            evidence,
            session,
            *authorize_execution,
        ),
        BridgeCommand::Plan { file, .. } => plan_report(file, session),
        BridgeCommand::Inspect { state, run_id, .. } => {
            if state.is_symlink_lexical() || !state.is_dir() {
                return fail("State root must be an existing real directory");
            }
            Ok(json!({
                "session_id": session,
                "mode": "read-only",
                "result": to_value(&inspect(state, run_id)?)?,
            }))
        }
        _ => fail("Unknown bridge operation"),
    }
}

/// Dispatch only explicitly authorized actions; never merge or execute generated code.
///
/// # Errors
/// Any refusal or failure of the selected operation.
pub fn handle(args: &Args) -> Result<Value, BridgeError> {
    handle_with(args, &apply_reviewed_receipt)
}

/// Handle a command with a caller-supplied receipt application backend.
///
/// # Errors
/// Same as [`handle`].
pub fn handle_with(args: &Args, apply: ApplyReviewed<'_>) -> Result<Value, BridgeError> {
    let command = &args.command;
    let Some(session) = session_of(command) else {
        return Ok(doctor());
    };
    let session = check_session(session)?;
    if let Some(result) = workflow(command, session)? {
        return Ok(result);
    }
    if let Some(result) = repair(command, session)? {
        return Ok(result);
    }
    if let Some(result) = live(command, session)? {
        return Ok(result);
    }
    if let Some(result) = evidence_commands(command, session, apply)? {
        return Ok(result);
    }
    simple(command, session)
}

/// Printed success line: `json.dumps({'ok': True, **output}, sort_keys=True)`.
///
/// # Errors
/// The result cannot be encoded.
pub fn render(output: &Value) -> Result<String, JsonError> {
    let ok = Value::Bool(true);
    let mut report = BTreeMap::from([("ok", &ok)]);
    if let Value::Object(fields) = output {
        report.extend(fields.iter().map(|(key, value)| (key.as_str(), value)));
    } else {
        report.insert("result", output);
    }
    sorted_json_spaced_fields(&report)
}

/// Printed failure line (the process exits with status 2).
///
/// # Errors
/// The message cannot be encoded.
pub fn render_error(error: &BridgeError) -> Result<String, JsonError> {
    sorted_json_spaced_value(&json!({"error": error.to_string(), "ok": false}))
}

/// Run one parsed command and return the text Python prints.
///
/// # Errors
/// [`BridgeError`]; callers print [`render_error`] and exit with status 2.
pub fn run(args: &Args) -> Result<String, BridgeError> {
    Ok(render(&handle(args)?)?)
}
