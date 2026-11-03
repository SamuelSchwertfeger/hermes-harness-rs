//! Hermes-native CLI skills and independently authorized local-model adapters.
//!
//! Results are [`serde_json::Value`] objects whose keys serialise sorted, matching the
//! Python `json.dumps(result, sort_keys=True)` form the native CLI prints.

use crate::multiagent::runtime::fail;
use crate::util::PathLexical as _;
use crate::util::{is_py_blank, is_slug_id};
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::git::worktrees::resolve_strict;
use crate::multiagent::apply_edits::{ApplyEditsError, apply_new_files};
use crate::multiagent::local_inference::{
    LocalEndpoint, LocalInferenceError, Transport, propose_edits, propose_plan,
};
use crate::multiagent::model_edits::EditProposal;
use crate::multiagent::model_pipeline::{ModelPipelineError, generate_and_review};
use crate::multiagent::ollama_discovery::{DEFAULT_BASE_URL, ollama_models};
use crate::multiagent::plan_io::parse_plan_json;
use crate::multiagent::replacement_pipeline::{
    ReplacementPipelineError, ReplacementTask, generate_reviewed_replacements,
};
use crate::multiagent::routing::{OllamaRouting, qualify_models};
use crate::multiagent::runtime::{
    DemoFactory, ExecutionConfig, Node, Plan, Role, WorkflowError, execute, inspect, ordered_nodes,
};
use crate::util::ordered_map::OrderedMap;
use crate::util::{create_private_dir, resolve_lenient};

const SAMPLE_LIMIT: usize = 40;
const MAX_PLAN_BYTES: u64 = 65_536;
/// Maximum accepted length of an originating Hermes session identifier.
pub const MAX_SESSION_ID_LENGTH: usize = 128;
const REPO_TIMEOUT: Duration = Duration::from_secs(10);
const GIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Failure of a native workflow that may come from several lower layers.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// Workflow rule violation or filesystem/JSON failure.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Local-model inference failure.
    #[error(transparent)]
    Inference(#[from] LocalInferenceError),
    /// Propose-and-review pipeline failure.
    #[error(transparent)]
    ModelPipeline(#[from] ModelPipelineError),
    /// Replacement pipeline failure.
    #[error(transparent)]
    ReplacementPipeline(#[from] ReplacementPipelineError),
    /// Failure while applying new files to a worktree.
    #[error(transparent)]
    ApplyEdits(#[from] ApplyEditsError),
}

impl NativeError {
    /// Whether this is an exclusive-create collision (Python `FileExistsError`).
    #[must_use]
    pub fn is_already_exists(&self) -> bool {
        matches!(self, Self::Workflow(error) if error.is_already_exists())
    }
}

fn denied<T>(message: &str) -> Result<T, NativeError> {
    Err(WorkflowError::msg(message).into())
}

fn value_of<T: Serialize>(item: &T) -> Result<Value, WorkflowError> {
    Ok(serde_json::to_value(item)?)
}

struct GitOutput {
    success: bool,
    stdout: String,
}

/// Run `git -C root args...` without a shell, killing it after `timeout`.
fn run_git(root: &Path, args: &[&str], timeout: Duration) -> Result<GitOutput, WorkflowError> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                WorkflowError::msg("Git is not installed")
            } else {
                WorkflowError::Io(error)
            }
        })?;
    let mut pipe = child.stdout.take();
    let reader = thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(stream) = pipe.as_mut() {
            let _ = stream.read_to_end(&mut buffer);
        }
        buffer
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(WorkflowError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "git command timed out",
            )));
        }
        thread::sleep(Duration::from_millis(2));
    };
    let bytes = reader.join().unwrap_or_default();
    Ok(GitOutput {
        success: status.success(),
        stdout: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// Require an existing, real Git checkout (never create or clone).
///
/// # Errors
/// The path is a symlink or not a directory, Git is missing, or the path is not the root of a
/// Git checkout.
pub fn repository(path: &Path) -> Result<PathBuf, WorkflowError> {
    if path.is_symlink_lexical() || !path.is_dir() {
        return fail("Repository must be a real directory");
    }
    let root = resolve_strict(path)?;
    let top = run_git(&root, &["rev-parse", "--show-toplevel"], REPO_TIMEOUT)?;
    let reported = PathBuf::from(crate::util::py_strip(&top.stdout));
    let reported = resolve_strict(&reported).unwrap_or_else(|_| resolve_lenient(&reported));
    if !top.success || reported != root {
        return fail("Path must be the root of a Git checkout");
    }
    Ok(root)
}

/// Run a bounded read-only Git command in `root` and return its standard output.
///
/// # Errors
/// Git is missing, times out, or exits non-zero.
pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String, WorkflowError> {
    let output = run_git(root, args, GIT_TIMEOUT)?;
    if !output.success {
        return fail("Git inspection failed");
    }
    Ok(output.stdout)
}

fn sample(lines: &str) -> (usize, Vec<&str>) {
    let mut count = 0;
    let mut sampled = Vec::new();
    for line in lines.lines() {
        if count < SAMPLE_LIMIT {
            sampled.push(line);
        }
        count += 1;
    }
    (count, sampled)
}

/// Return read-only bounded evidence, without reading source or secrets.
///
/// # Errors
/// The path is not a Git checkout root or a Git inspection fails.
pub fn investigate(repo: &Path) -> Result<Value, WorkflowError> {
    let root = repository(repo)?;
    let tracked = git(&root, &["ls-files", "--"])?;
    let status = git(&root, &["status", "--short", "--untracked-files=no"])?;
    let (tracked_count, tracked_sample) = sample(&tracked);
    let (status_count, status_sample) = sample(&status);
    Ok(json!({
        "action": "lets-investigate",
        "mode": "read-only",
        "repo": root.to_string_lossy(),
        "head": crate::util::py_strip(&git(&root, &["rev-parse", "HEAD"])?),
        "branch": crate::util::py_strip(&git(&root, &["branch", "--show-current"])?),
        "tracked_count": tracked_count,
        "tracked_sample": tracked_sample,
        "tracked_truncated": tracked_count > SAMPLE_LIMIT,
        "changes": status_sample,
        "changes_truncated": status_count > SAMPLE_LIMIT,
        "note": "Metadata-only investigation; no source files read or modified",
    }))
}

fn string_list(value: Option<&Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn parse_node(row: &Value) -> Result<Node, WorkflowError> {
    let Value::Object(row) = row else {
        return fail("Each node must be a JSON object");
    };
    let id = row.get("id").and_then(Value::as_str);
    let objective = row.get("objective").and_then(Value::as_str);
    let paths = string_list(row.get("owned_paths"));
    let deps = match row.get("dependencies") {
        None => Some(Vec::new()),
        other => string_list(other),
    };
    let (Some(id), Some(objective), Some(owned_paths), Some(dependencies)) =
        (id, objective, paths, deps)
    else {
        return fail("Invalid node fields");
    };
    Ok(Node {
        id: id.to_owned(),
        objective: objective.to_owned(),
        owned_paths,
        dependencies,
        role: Role::Coding,
    })
}

/// Reject invalid plan shapes before calling the canonical DAG validator.
///
/// # Errors
/// The document is not a well-formed plan, or the DAG validator rejects it.
pub fn parse_plan(data: &Value) -> Result<Plan, WorkflowError> {
    let Value::Object(data) = data else {
        return fail("Plan must be a JSON object");
    };
    let (Some(id), Some(objective), Some(Value::Array(rows))) = (
        data.get("id").and_then(Value::as_str),
        data.get("objective").and_then(Value::as_str),
        data.get("nodes"),
    ) else {
        return fail("Plan needs id, objective and nodes");
    };
    let nodes = rows.iter().map(parse_node).collect::<Result<Vec<_>, _>>()?;
    let primary_identity = match data.get("primary_identity") {
        None => "offline-primary",
        Some(Value::String(identity)) => identity,
        Some(_) => return fail("Invalid primary identity"),
    };
    let plan = Plan {
        id: id.to_owned(),
        objective: objective.to_owned(),
        nodes,
        primary_identity: primary_identity.to_owned(),
    };
    ordered_nodes(&plan)?;
    Ok(plan)
}

fn is_bounded_plan_file(file: &Path) -> bool {
    !file.is_symlink_lexical()
        && file.is_file()
        && file
            .metadata()
            .is_ok_and(|meta| meta.len() <= MAX_PLAN_BYTES)
}

/// Validate an operator/primary supplied JSON plan; do not fabricate a model plan.
///
/// This is the file-based form of Python `game_plan(Path)`.
///
/// # Errors
/// The file is not a real JSON file of at most 64KiB, is not valid JSON, or is not a valid plan.
pub fn game_plan_file(file: &Path) -> Result<Value, WorkflowError> {
    if !is_bounded_plan_file(file) {
        return fail("Plan must be a real JSON file of at most 64KiB");
    }
    let bytes = fs::read(file)?;
    let text = String::from_utf8(bytes).map_err(|_| WorkflowError::msg("Invalid plan JSON"))?;
    let document: Value =
        serde_json::from_str(&text).map_err(|source| WorkflowError::AdmissionJson {
            message: "Invalid plan JSON".to_owned(),
            source,
        })?;
    let plan = parse_plan(&document)?;
    let order: Vec<String> = ordered_nodes(&plan)?
        .into_iter()
        .map(|node| node.id)
        .collect();
    Ok(json!({
        "action": "game-plan",
        "mode": "offline-validated",
        "plan": value_of(&plan)?,
        "execution_order": order,
        "note": "Plan supplied externally; no primary model invoked",
    }))
}

/// `Path.mkdir(mode=0o700, parents=True, exist_ok=True)`.
fn make_private_dir(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    create_private_dir(path)
}

/// Execute canonical deterministic doubles; no model/network/merge.
///
/// # Errors
/// Invalid repository, plan file or state directory, an existing run directory
/// ([`WorkflowError::is_already_exists`]), or an execution failure.
pub fn assemble(repo: &Path, state: &Path, file: &Path) -> Result<Value, WorkflowError> {
    let root = repository(repo)?;
    if !is_bounded_plan_file(file) {
        return fail("Invalid plan file");
    }
    let document: Value = serde_json::from_slice(&fs::read(file)?)?;
    let plan = parse_plan(&document)?;
    if state.is_symlink_lexical() || (state.exists() && !state.is_dir()) {
        return fail("Invalid state directory");
    }
    make_private_dir(state)?;
    let config = ExecutionConfig::new(root, &DemoFactory);
    let result = execute(&resolve_lenient(state), &plan, &config)?;
    Ok(json!({
        "action": "agents-assemble",
        "mode": "offline-simulated",
        "simulated_agents": true,
        "result": value_of(&result)?,
        "note": "Deterministic doubles only; no live model, integration, merge or push",
    }))
}

/// Read-only inspection of a persisted run.
///
/// # Errors
/// Invalid run ID, or a missing or malformed persisted record.
pub fn inspect_run(state: &Path, run_id: &str) -> Result<Value, WorkflowError> {
    if !is_slug_id(run_id) {
        return fail("Invalid run ID");
    }
    Ok(json!({"action": "inspect", "result": value_of(&inspect(state, run_id)?)?}))
}

/// Trusted adapter context: originating session, repository and state root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeContext {
    /// Originating Hermes session identifier.
    pub session_id: String,
    /// Repository directory.
    pub repository: PathBuf,
    /// Directory holding persisted run state.
    pub state_root: PathBuf,
}

impl NativeContext {
    /// Bundle the originating session, repository and state root.
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        repository: impl Into<PathBuf>,
        state_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            repository: repository.into(),
            state_root: state_root.into(),
        }
    }
}

fn validate_context(context: &NativeContext) -> Result<(), WorkflowError> {
    if is_py_blank(&context.session_id)
        || context.session_id.chars().count() > MAX_SESSION_ID_LENGTH
    {
        return fail("Invalid originating Hermes session");
    }
    if context.repository.is_symlink_lexical() || !context.repository.is_dir() {
        return fail("Repository must be an existing real directory");
    }
    if context.state_root.is_symlink_lexical() || !context.state_root.is_dir() {
        return fail("State root must be an existing real directory");
    }
    Ok(())
}

fn validated(context: &NativeContext, plan: &Plan) -> Result<Map<String, Value>, WorkflowError> {
    validate_context(context)?;
    let ordered = ordered_nodes(plan)?;
    let roles: Map<String, Value> = ordered
        .iter()
        .map(|node| (node.id.clone(), json!(node.role.as_str())))
        .collect();
    let ids: Vec<&str> = ordered.iter().map(|node| node.id.as_str()).collect();
    let mut result = Map::new();
    result.insert("session_id".to_owned(), json!(context.session_id));
    result.insert("plan_id".to_owned(), json!(plan.id));
    result.insert("ordered_nodes".to_owned(), json!(ids));
    result.insert("roles".to_owned(), Value::Object(roles));
    result.insert("mode".to_owned(), json!("validation-only"));
    Ok(result)
}

/// Validate a canonical bounded plan with no dispatch or mutations.
///
/// # Errors
/// Invalid context or plan.
pub fn validate_native_plan(context: &NativeContext, plan: &Plan) -> Result<Value, WorkflowError> {
    validated(context, plan).map(Value::Object)
}

/// Require explicit offline authorization; persist auditable simulated execution.
///
/// This entry point refuses Cyber Warrior dispatch: trusted `CyberGrant` transport
/// and native Hermes caller permissions are not yet host-qualified.
///
/// # Errors
/// Invalid context, missing authorization, a Cyber Warrior node, or an execution failure.
pub fn agents_assemble_offline(
    context: &NativeContext,
    plan: &Plan,
    authorized: bool,
) -> Result<Value, WorkflowError> {
    validate_context(context)?;
    if !authorized {
        return fail("Explicit offline authorization is required");
    }
    let ordered = ordered_nodes(plan)?;
    if ordered.iter().any(|node| node.role == Role::CyberWarrior) {
        return fail("Cyber Warrior native grant transport is not qualified");
    }
    let config = ExecutionConfig::new(context.repository.clone(), &DemoFactory);
    let result = execute(&context.state_root, plan, &config)?;
    Ok(json!({
        "session_id": context.session_id,
        "mode": "simulated-offline",
        "real_models": false,
        "result": value_of(&result)?,
    }))
}

/// Read persisted evidence without retrying, recovering, or approving work.
///
/// # Errors
/// Invalid context, invalid run ID, or a missing or malformed persisted record.
pub fn what_we_know(context: &NativeContext, run_id: &str) -> Result<Value, WorkflowError> {
    validate_context(context)?;
    Ok(json!({
        "session_id": context.session_id,
        "mode": "read-only",
        "result": value_of(&inspect(&context.state_root, run_id)?)?,
    }))
}

/// Validate Hermes primary model output before any authorization or dispatch.
///
/// # Errors
/// The payload is not an admissible plan, or the context is invalid.
pub fn admit_plan_json(context: &NativeContext, payload: &str) -> Result<Value, WorkflowError> {
    validate_native_plan(context, &parse_plan_json(payload)?)
}

/// Opt-in inference-only coordinator; no tools, worktrees, or dispatch.
///
/// # Errors
/// Invalid context, missing authorization, inference failure, or an invalid proposed plan.
pub fn propose_local_plan(
    context: &NativeContext,
    endpoint: &LocalEndpoint,
    task: &str,
    transport: &dyn Transport,
    authorized: bool,
) -> Result<Value, NativeError> {
    validate_context(context)?;
    if !authorized {
        return denied("Explicit local inference authorization required");
    }
    let plan = propose_plan(endpoint, task, transport)?;
    let mut result = validated(context, &plan)?;
    result.insert("mode".to_owned(), json!("local-model-plan-validation"));
    result.insert("model".to_owned(), json!(endpoint.model));
    result.insert("real_inference".to_owned(), json!(true));
    result.insert("worker_execution".to_owned(), json!(false));
    Ok(Value::Object(result))
}

fn file_digests(proposal: &EditProposal) -> Vec<Value> {
    proposal
        .files
        .iter()
        .map(|file| json!({"path": file.path, "sha256": file.sha256}))
        .collect()
}

/// Return scoped proposal digests, never edit files or execute generated text.
///
/// # Errors
/// Invalid context, missing authorization, unsupported role, or inference failure.
pub fn propose_local_edits(
    context: &NativeContext,
    endpoint: &LocalEndpoint,
    node: &Node,
    transport: &dyn Transport,
    authorized: bool,
) -> Result<Value, NativeError> {
    validate_context(context)?;
    if !authorized {
        return denied("Explicit local inference authorization required");
    }
    if node.role == Role::CyberWarrior {
        return denied("Cyber Warrior trusted grant transport is unqualified");
    }
    let result = propose_edits(
        endpoint,
        &node.id,
        &node.objective,
        &node.owned_paths,
        transport,
    )?;
    Ok(json!({
        "session_id": context.session_id,
        "mode": "local-model-proposal-only",
        "task_id": result.task_id,
        "files": file_digests(&result),
        "applied": false,
        "reviewed": false,
    }))
}

/// Apply an already admitted proposal to a caller-selected disposable worktree.
///
/// This is not live autonomous worker dispatch or a substitute for OS isolation.
///
/// # Errors
/// Invalid context, or any failure of the audited new-file application.
pub fn apply_local_new_file_proposal(
    context: &NativeContext,
    workspace: &Path,
    proposal: &EditProposal,
    owned_paths: &[String],
    baseline: &str,
    authorized: bool,
) -> Result<Value, NativeError> {
    validate_context(context)?;
    let result = apply_new_files(workspace, proposal, owned_paths, baseline, authorized)?;
    Ok(json!({
        "session_id": context.session_id,
        "mode": "authorized-disposable-new-files",
        "task_id": result.task_id,
        "changed_paths": result.audit.changed_paths,
        "baseline": result.audit.baseline,
        "head": result.audit.head,
        "merged": false,
        "verified_by_tests": false,
    }))
}

/// Read-only model listing from this host's configured loopback Ollama.
///
/// # Errors
/// Invalid context, or a discovery failure.
pub fn discover_local_ollama_models(context: &NativeContext) -> Result<Value, NativeError> {
    validate_context(context)?;
    let models = ollama_models(DEFAULT_BASE_URL)?;
    let rows: Vec<Value> = models
        .iter()
        .map(|item| json!({"name": item.name, "size_bytes": item.size_bytes}))
        .collect();
    Ok(json!({
        "session_id": context.session_id,
        "mode": "read-only-model-discovery",
        "models": rows,
        "applied": false,
    }))
}

/// Discover installed Ollama models and verify selected serial role routing.
///
/// # Errors
/// Invalid context, discovery failure, or a role that cannot be routed.
pub fn qualify_local_roles(
    context: &NativeContext,
    roles: &[&str],
    routing: Option<&OllamaRouting>,
) -> Result<Value, NativeError> {
    validate_context(context)?;
    let default_routing = OllamaRouting::default();
    let selected = routing.unwrap_or(&default_routing);
    let inventory = ollama_models(&selected.base_url)?;
    let qualification = qualify_models(selected, &inventory, roles)?;
    let Value::Object(mut result) = value_of(&qualification)? else {
        return denied("Invalid qualification record");
    };
    result.insert("session_id".to_owned(), json!(context.session_id));
    result.insert("inference_executed".to_owned(), json!(false));
    result.insert("worker_tools_qualified".to_owned(), json!(false));
    Ok(Value::Object(result))
}

/// Return an independently reviewed proposal; do not apply any files.
///
/// # Errors
/// Invalid context, non-coding node, or any pipeline failure.
pub fn propose_and_review_local_coding(
    context: &NativeContext,
    node: &Node,
    worker_endpoint: &LocalEndpoint,
    reviewer_endpoint: &LocalEndpoint,
    transport: &dyn Transport,
    authorized: bool,
) -> Result<Value, NativeError> {
    validate_context(context)?;
    if node.role != Role::Coding {
        return denied("Only ordinary coding nodes supported by native model pipeline");
    }
    let result = generate_and_review(
        &node.id,
        &node.objective,
        &node.owned_paths,
        worker_endpoint,
        reviewer_endpoint,
        transport,
        authorized,
    )?;
    Ok(json!({
        "session_id": context.session_id,
        "task_id": node.id,
        "mode": "reviewed-proposal-only",
        "worker_model": worker_endpoint.model,
        "reviewer_model": reviewer_endpoint.model,
        "verdict": result.review.verdict.as_str(),
        "findings": result.review.findings,
        "files": file_digests(&result.proposal),
        "applied": false,
        "authorizes_merge": false,
    }))
}

/// Review preimage-bound replacement proposals; never apply generated content.
///
/// # Errors
/// Invalid context, non-coding node, or any pipeline failure.
pub fn propose_and_review_local_replacements(
    context: &NativeContext,
    node: &Node,
    worker_endpoint: &LocalEndpoint,
    reviewer_endpoint: &LocalEndpoint,
    preimages: &OrderedMap<String>,
    transport: &dyn Transport,
    authorized: bool,
) -> Result<Value, NativeError> {
    validate_context(context)?;
    if node.role != Role::Coding {
        return denied("Replacement pipeline supports only the coding worker role");
    }
    let task = ReplacementTask {
        task_id: &node.id,
        objective: &node.objective,
        owned_paths: &node.owned_paths,
        preimages,
    };
    let reviewed = generate_reviewed_replacements(
        worker_endpoint,
        reviewer_endpoint,
        &task,
        transport,
        authorized,
    )?;
    let files: Vec<Value> = reviewed
        .replacements
        .iter()
        .map(|item| json!({"path": item.path, "expected_sha256": item.expected_sha256}))
        .collect();
    Ok(json!({
        "session_id": context.session_id,
        "mode": "reviewed-replacements-only",
        "task_id": node.id,
        "worker_model": worker_endpoint.model,
        "reviewer_model": reviewer_endpoint.model,
        "verdict": reviewed.verdict.as_str(),
        "findings": reviewed.findings,
        "files": files,
        "applied": false,
        "authorizes_merge": false,
    }))
}

/// Context-based native form of Python `game_plan(context, plan)`.
///
/// # Errors
/// Invalid context or plan.
pub fn game_plan(context: &NativeContext, plan: &Plan) -> Result<Value, WorkflowError> {
    validate_native_plan(context, plan)
}
