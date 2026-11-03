//! One-shot, reservation-bound local replacement inference for failed tests.
//!
//! A dispatch claim is created *before* either model request; interruptions remain visible
//! and are never retried automatically. No changes are applied or executed.

use crate::util::PathLexical as _;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::functional_verify::audit_checkout;
use super::lifecycle::{files, load};
use super::repair_controller::{
    MAX_REPAIR_FILES, attempts_used, inspect_repair, require, resolved, sequence, text_of,
};
use crate::deployment::gpu::write_once_value as write_once;
use crate::deployment::gpu::{gpu_lane, load_gpu_policy};
use crate::deployment::providers::ProviderSpec;
use crate::deployment::providers::{ConfiguredEndpoint, ProviderError, load_registry};
use crate::multiagent::local_inference::{LocalEndpoint, LocalInferenceError, Transport};
use crate::multiagent::ollama_native::{
    NativeOllamaJsonTransport, Schema, closed_object, review_schema, string_property,
    task_proposal_schema,
};
use crate::multiagent::replacement_pipeline::{
    ReplacementPipelineError, ReplacementReview, ReplacementTask, generate_reviewed_replacements,
};
use crate::multiagent::runtime::WorkflowError;
use crate::util::ordered_map::OrderedMap;
use crate::util::{JsonError, ordered_json_spaced, py_path_str, sha256_hex};

/// Longest repair objective in characters.
pub const MAX_REPAIR_OBJECTIVE_LENGTH: usize = 4096;
/// Most attempts one repair sequence may reserve.
pub const MAX_REPAIR_ATTEMPTS: usize = 3;
/// Largest repair source file in bytes.
pub const MAX_REPAIR_SOURCE_BYTES: usize = 8192;
/// Largest combined JSON of the source context in characters.
pub const MAX_REPAIR_CONTEXT_LENGTH: usize = 12000;

const URL: &str = "http://127.0.0.1:11434/v1/chat/completions";

/// Failure of a repair dispatch step.
#[derive(Debug, thiserror::Error)]
pub enum RepairDispatchError {
    /// Workflow rule violation or filesystem/JSON failure.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Provider registry failure.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// Local inference configuration failure.
    #[error(transparent)]
    Inference(#[from] LocalInferenceError),
    /// Worker or reviewer request failure.
    #[error(transparent)]
    Pipeline(#[from] ReplacementPipelineError),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

fn fail<T>(message: &str) -> Result<T, RepairDispatchError> {
    Err(WorkflowError::msg(message).into())
}

/// Inspect a dispatch without invoking either model.
///
/// # Errors
/// An unknown task, invalid history, or a dispatch claim or review that is malformed or not
/// linked to this attempt.
pub fn inspect_dispatch(state: &Path, task_id: &str) -> Result<Value, WorkflowError> {
    let (root, _) = sequence(state, task_id)?;
    let report = inspect_repair(state, task_id)?;
    let used = match attempts_used(&report) {
        Some(used) if used > 0 => used,
        _ => return Ok(json!({"status": "not-reserved", "task_id": task_id})),
    };
    let fence = root.join(format!("{task_id}-repair-{used}-dispatch.json"));
    let review = root.join(format!("{task_id}-repair-{used}-review.json"));
    if !fence.exists() && !fence.is_symlink_lexical() {
        return Ok(json!({"status": "not-dispatched", "task_id": task_id, "number": used}));
    }
    let (claim, claim_hash) = load(&fence)?;
    require(
        claim.get("mode").and_then(Value::as_str) == Some("repair-dispatch-claim")
            && claim.get("task_id").and_then(Value::as_str) == Some(task_id)
            && claim.get("number") == Some(&json!(used)),
        "Invalid dispatch claim",
    )?;
    if !review.exists() && !review.is_symlink_lexical() {
        return Ok(json!({
            "status": "dispatched-unresolved",
            "claim_sha256": claim_hash,
            "task_id": task_id,
            "number": used,
            "no_automatic_retry": true,
        }));
    }
    let (result, digest) = load(&review)?;
    require(
        result.get("mode").and_then(Value::as_str) == Some("repair-independent-review")
            && result.get("claim_sha256").and_then(Value::as_str) == Some(claim_hash.as_str())
            && result.get("reservation_sha256") == claim.get("reservation_sha256")
            && result.get("task_id").and_then(Value::as_str) == Some(task_id)
            && result.get("number") == Some(&json!(used)),
        "Invalid dispatch review linkage",
    )?;
    Ok(json!({
        "status": "review-recorded",
        "task_id": task_id,
        "number": used,
        "review": result,
        "review_sha256": digest,
        "review_path": py_path_str(&review),
    }))
}

/// Inputs of one dispatch.
#[derive(Clone, Copy)]
pub struct DispatchRequest<'a> {
    /// State directory.
    pub state: &'a Path,
    /// Task identifier.
    pub task_id: &'a str,
    /// Integrity checkpoint the failed attempt is bound to.
    pub checkpoint: &'a Path,
    /// What the replacement must achieve.
    pub objective: &'a str,
    /// Checkpoint-owned files the replacements may touch.
    pub owned_paths: &'a [String],
    /// Explicit local model inference authorization.
    pub authorized: bool,
    /// Worker endpoint override (forbidden together with a registry).
    pub worker: Option<&'a LocalEndpoint>,
    /// Reviewer endpoint override (forbidden together with a registry).
    pub reviewer: Option<&'a LocalEndpoint>,
    /// Transport override.
    pub transport: Option<&'a dyn Transport>,
    /// Operator provider registry.
    pub registry_file: Option<&'a Path>,
    /// Operator GPU policy.
    pub gpu_policy_file: Option<&'a Path>,
}

impl std::fmt::Debug for DispatchRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchRequest")
            .field("state", &self.state)
            .field("task_id", &self.task_id)
            .field("authorized", &self.authorized)
            .finish_non_exhaustive()
    }
}

impl<'a> DispatchRequest<'a> {
    /// Request with no endpoint, transport, registry or GPU policy overrides.
    #[must_use]
    pub const fn new(
        state: &'a Path,
        task_id: &'a str,
        checkpoint: &'a Path,
        objective: &'a str,
        owned_paths: &'a [String],
    ) -> Self {
        Self {
            state,
            task_id,
            checkpoint,
            objective,
            owned_paths,
            authorized: false,
            worker: None,
            reviewer: None,
            transport: None,
            registry_file: None,
            gpu_policy_file: None,
        }
    }
}

/// Host effects used by repair dispatch.
///
/// Implement this trait to supply host operations to [`dispatch_repair_with`].
pub trait DispatchHost {
    /// Revalidate the checkout (Python `_audit_checkout`).
    ///
    /// # Errors
    /// Same as [`audit_checkout`].
    fn audit(
        &self,
        root: &Path,
        record: &Map<String, Value>,
    ) -> Result<BTreeMap<String, String>, WorkflowError>;
    /// Credential lookup by environment reference (Python `os.environ`).
    fn credential(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
    /// Host name (Python `socket.gethostname()`).
    fn hostname(&self) -> String;
    /// Worker proposal and independent review (Python `generate_reviewed_replacements`).
    ///
    /// # Errors
    /// Same as [`generate_reviewed_replacements`].
    fn generate(
        &self,
        worker: &LocalEndpoint,
        reviewer: &LocalEndpoint,
        task: &ReplacementTask<'_>,
        transport: &dyn Transport,
    ) -> Result<ReplacementReview, ReplacementPipelineError>;
    /// Whether `content` parses as Python source (Python `ast.parse`, `SyntaxError` -> false).
    ///
    /// # Errors
    /// No Python interpreter could answer.
    fn python_parses(&self, name: &str, content: &str) -> Result<bool, WorkflowError>;
}

/// The real host: Git audit, host name, local inference and a `python3` syntax check.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHost;

const SYNTAX_PROGRAM: &str = "import ast, sys\n\
try:\n    ast.parse(sys.stdin.buffer.read().decode('utf-8'), filename=sys.argv[1])\n\
except SyntaxError:\n    sys.exit(3)\n";

impl DispatchHost for SystemHost {
    fn audit(
        &self,
        root: &Path,
        record: &Map<String, Value>,
    ) -> Result<BTreeMap<String, String>, WorkflowError> {
        audit_checkout(root, record)
    }

    fn hostname(&self) -> String {
        #[cfg(unix)]
        {
            rustix::system::uname()
                .nodename()
                .to_string_lossy()
                .into_owned()
        }
        #[cfg(not(unix))]
        {
            Command::new("hostname")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|name| name.trim().to_owned())
                .unwrap_or_default()
        }
    }

    fn generate(
        &self,
        worker: &LocalEndpoint,
        reviewer: &LocalEndpoint,
        task: &ReplacementTask<'_>,
        transport: &dyn Transport,
    ) -> Result<ReplacementReview, ReplacementPipelineError> {
        generate_reviewed_replacements(worker, reviewer, task, transport, true)
    }

    fn python_parses(&self, name: &str, content: &str) -> Result<bool, WorkflowError> {
        let unavailable = || WorkflowError::msg("Python syntax checker unavailable");
        #[cfg(unix)]
        let python = "/usr/bin/python3";
        #[cfg(not(unix))]
        let python = "python3";
        let mut child = Command::new(python)
            .args(["-I", "-c", SYNTAX_PROGRAM, name])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| unavailable())?;
        if let Some(mut stdin) = child.stdin.take() {
            // A broken pipe means the interpreter already exited; its status decides.
            let _ = stdin.write_all(content.as_bytes());
        }
        match child.wait().map_err(|_| unavailable())?.code() {
            Some(0) => Ok(true),
            Some(3) => Ok(false),
            _ => Err(unavailable()),
        }
    }
}

/// Wire schema for replacement proposals (Python `_REPLACEMENT_SCHEMA`).
fn replacement_schema() -> Schema {
    task_proposal_schema(
        "replacements",
        closed_object(
            [
                ("path", string_property()),
                ("expected_sha256", string_property()),
                ("content", string_property()),
            ],
            ["path", "expected_sha256", "content"],
        ),
    )
}

/// Endpoints and transport chosen for one dispatch.
struct Selected {
    worker: LocalEndpoint,
    reviewer: LocalEndpoint,
    transport: Option<Box<dyn Transport>>,
}

#[derive(Serialize)]
struct DispatchPrompt<'a> {
    objective: &'a str,
    existing_files: &'a OrderedMap<String>,
}

/// Registry-bound endpoints with GPU admission; returns the pair and the policy if any.
fn registry_endpoints(
    host: &dyn DispatchHost,
    request: &DispatchRequest<'_>,
    registry_file: &Path,
    context_chars: usize,
) -> Result<
    (
        ConfiguredEndpoint,
        ConfiguredEndpoint,
        Option<crate::deployment::gpu::GPUPolicy>,
    ),
    RepairDispatchError,
> {
    let registry = load_registry(registry_file)?;
    let policy = request.gpu_policy_file.map(load_gpu_policy).transpose()?;
    require(
        request.worker.is_none() && request.reviewer.is_none(),
        "Explicit registry forbids unregistered endpoint overrides",
    )?;
    let worker = registry.endpoint("repair-worker")?;
    let reviewer = registry.endpoint("repair-reviewer")?;
    let selected: [&ProviderSpec; 2] = [
        registry.role("repair-worker")?,
        registry.role("repair-reviewer")?,
    ];
    let prompt_chars = i64::try_from(request.objective.chars().count() + context_chars)
        .map_err(|_| WorkflowError::msg("Invalid repair objective"))?;
    require(
        selected
            .iter()
            .all(|spec| spec.context_tokens >= prompt_chars + 2048 + 512),
        "Configured model context insufficient for bounded repair input",
    )?;
    let remote_hosts: Vec<&str> = selected
        .iter()
        .map(|spec| spec.gpu_host.as_str())
        .filter(|name| !name.is_empty())
        .collect();
    if let Some(first) = remote_hosts.first() {
        let single = remote_hosts.iter().all(|name| name == first);
        let Some(active) = policy.as_ref().filter(|p| single && p.host == *first) else {
            return fail("Remote GPU registry requires matching operator policy");
        };
        let units: Vec<&str> = selected
            .iter()
            .map(|spec| spec.unit.as_str())
            .filter(|unit| !unit.is_empty())
            .collect();
        active.admit(&units)?;
        if host.hostname() != active.host {
            require(
                !active.units.iter().any(|unit| unit.managed),
                "Remote services must remain externally managed; no distributed lease",
            )?;
        }
    }
    Ok((worker, reviewer, policy))
}

fn read_sources(
    host: &dyn DispatchHost,
    workspace: &Path,
    record: &Map<String, Value>,
    owned: &[String],
    checkpoint_files: &BTreeMap<String, String>,
) -> Result<OrderedMap<String>, RepairDispatchError> {
    let actual = host.audit(workspace, record)?;
    require(&actual == checkpoint_files, "Checkpoint worktree drift")?;
    let root = resolved(workspace);
    let mut context = OrderedMap::new();
    for name in owned {
        let target = workspace.join(name);
        require(
            target.is_file()
                && !target.is_symlink_lexical()
                && resolved(&target).starts_with(&root),
            "Unsafe repair source file",
        )?;
        let raw = std::fs::read(&target).map_err(WorkflowError::from)?;
        require(
            raw.len() <= MAX_REPAIR_SOURCE_BYTES,
            "Repair source exceeds context budget",
        )?;
        require(
            checkpoint_files.get(name).map(String::as_str) == Some(sha256_hex(&raw).as_str()),
            "Stale repair source preimage",
        )?;
        let Ok(text) = String::from_utf8(raw) else {
            return fail("Only UTF-8 repair source is supported");
        };
        context.insert(name.clone(), text);
    }
    let encoded = ordered_json_spaced(&context)?;
    require(
        encoded.len() <= MAX_REPAIR_CONTEXT_LENGTH,
        "Combined repair source exceeds context budget",
    )?;
    Ok(context)
}

fn select_endpoints(
    host: &dyn DispatchHost,
    request: &DispatchRequest<'_>,
    context_chars: usize,
) -> Result<(Selected, Option<crate::deployment::gpu::GPUPolicy>), RepairDispatchError> {
    let mut policy = request.gpu_policy_file.map(load_gpu_policy).transpose()?;
    let (worker, reviewer, transport) = if let Some(file) = request.registry_file {
        let (worker, reviewer, loaded) = registry_endpoints(host, request, file, context_chars)?;
        policy = loaded;
        require(
            worker.endpoint.model != reviewer.endpoint.model,
            "Worker/reviewer models must differ",
        )?;
        let credentials = |name: &str| host.credential(name);
        worker.validate_with(&credentials)?;
        reviewer.validate_with(&credentials)?;
        let registry = load_registry(file)?;
        let mut schemas = BTreeMap::new();
        schemas.insert("repair-worker".to_owned(), replacement_schema());
        schemas.insert("repair-reviewer".to_owned(), review_schema());
        let routed: Option<Box<dyn Transport>> = if request.transport.is_none() {
            Some(Box::new(registry.transport(
                &["repair-worker", "repair-reviewer"],
                &schemas,
            )?))
        } else {
            None
        };
        (worker.endpoint, reviewer.endpoint, routed)
    } else {
        let worker = request.worker.cloned().unwrap_or_else(|| {
            LocalEndpoint::new(URL, "qwen2.5-coder:7b")
                .with_timeout_seconds(120.0)
                .with_max_tokens(2048)
        });
        let reviewer = request.reviewer.cloned().unwrap_or_else(|| {
            LocalEndpoint::new(URL, "qwen3:4b")
                .with_timeout_seconds(120.0)
                .with_max_tokens(512)
        });
        require(
            worker.model != reviewer.model,
            "Worker/reviewer models must differ",
        )?;
        worker.validate()?;
        reviewer.validate()?;
        (worker, reviewer, None)
    };
    let transport = match (request.transport, transport) {
        (None, None) => Some(Box::new(
            NativeOllamaJsonTransport::new()
                .with_schema(&worker.model, replacement_schema())
                .with_schema(&reviewer.model, review_schema()),
        ) as Box<dyn Transport>),
        (_, routed) => routed,
    };
    Ok((
        Selected {
            worker,
            reviewer,
            transport,
        },
        policy,
    ))
}

/// Dispatch exactly once from the currently reserved failed-verification attempt.
///
/// # Errors
/// Missing authorization, an invalid objective, paths, checkpoint or worktree, models that do
/// not differ, an attempt that was already dispatched, inference failure, or a worktree that
/// changed during inference (the dispatch then stays unresolved and is never retried).
pub fn dispatch_repair(request: &DispatchRequest<'_>) -> Result<Value, RepairDispatchError> {
    dispatch_repair_with(&SystemHost, request)
}

/// Run [`dispatch_repair`] with caller-supplied host effects.
///
/// # Errors
/// Same as [`dispatch_repair`].
#[allow(clippy::too_many_lines)]
pub fn dispatch_repair_with(
    host: &dyn DispatchHost,
    request: &DispatchRequest<'_>,
) -> Result<Value, RepairDispatchError> {
    let DispatchRequest {
        state,
        task_id,
        checkpoint,
        objective,
        owned_paths,
        ..
    } = *request;
    require(
        request.authorized,
        "Explicit local model inference authorization required",
    )?;
    let objective_chars = objective.chars().count();
    require(
        0 < objective_chars && objective_chars <= MAX_REPAIR_OBJECTIVE_LENGTH,
        "Invalid repair objective",
    )?;
    let (root, opened) = sequence(state, task_id)?;
    let report = inspect_repair(state, task_id)?;
    require(
        text_of(&report, "next_stage") == Some("await-attempt-disposition"),
        "No reserved repair attempt",
    )?;
    let number = match attempts_used(&report) {
        Some(number) if (1..=MAX_REPAIR_ATTEMPTS).contains(&number) => number,
        _ => return fail("Invalid attempt number"),
    };
    let (reservation, reservation_hash) =
        load(&root.join(format!("{task_id}-repair-{number}.json")))?;
    let (original, checkpoint_hash) = load(checkpoint)?;
    require(
        opened.get("checkpoint_sha256").and_then(Value::as_str) == Some(checkpoint_hash.as_str())
            && reservation.get("checkpoint_sha256").and_then(Value::as_str)
                == Some(checkpoint_hash.as_str()),
        "Checkpoint does not match repair reservation",
    )?;
    require(
        original.get("task_id").and_then(Value::as_str) == Some(task_id)
            && original.get("baseline") == opened.get("baseline")
            && original.get("workspace") == opened.get("workspace"),
        "Repair identity mismatch",
    )?;
    let Some(workspace_name) = original.get("workspace").and_then(Value::as_str) else {
        return fail("Invalid worktree");
    };
    let workspace = PathBuf::from(workspace_name);
    require(
        workspace.is_dir() && !workspace.is_symlink_lexical(),
        "Worktree unavailable",
    )?;
    let checkpoint_files = files(&original)?;
    let distinct = owned_paths
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == owned_paths.len();
    require(
        !owned_paths.is_empty()
            && owned_paths.len() <= MAX_REPAIR_FILES
            && distinct
            && owned_paths
                .iter()
                .all(|path| checkpoint_files.contains_key(path)),
        "Repair paths must be explicit existing checkpoint-owned files",
    )?;
    let source_context = read_sources(host, &workspace, &original, owned_paths, &checkpoint_files)?;
    let mut preimages = OrderedMap::new();
    for name in owned_paths {
        if let Some(digest) = checkpoint_files.get(name) {
            preimages.insert(name.clone(), digest.clone());
        }
    }
    let context_chars = ordered_json_spaced(&source_context)?.len();
    let (selected, policy) = select_endpoints(host, request, context_chars)?;
    let claim = json!({
        "mode": "repair-dispatch-claim",
        "task_id": task_id,
        "number": number,
        "reservation_sha256": reservation_hash,
        "checkpoint_sha256": checkpoint_hash,
        "source_failure_sha256": opened.get("failure_sha256").cloned().ok_or_else(|| WorkflowError::msg("'failure_sha256'"))?,
        "baseline": opened.get("baseline").cloned().ok_or_else(|| WorkflowError::msg("'baseline'"))?,
        "workspace": workspace_name,
        "owned_paths": owned_paths,
        "preimages": &preimages,
        "objective_sha256": sha256_hex(objective.as_bytes()),
        "worker_model": selected.worker.model,
        "reviewer_model": selected.reviewer.model,
        "inference_granted": true,
        "application_granted": false,
        "execution_granted": false,
    });
    let fence = root.join(format!("{task_id}-repair-{number}-dispatch.json"));
    let claim_hash = match write_once(&fence, &claim) {
        Ok(digest) => digest,
        Err(error) if error.is_already_exists() => {
            return fail(
                "Repair attempt already dispatched; inspect saved evidence, never redispatch",
            );
        }
        Err(error) => return Err(error.into()),
    };
    let _lane = match &policy {
        Some(active) if host.hostname() == active.host => Some(gpu_lane(active)?),
        _ => None,
    };
    let prompt = ordered_json_spaced(&DispatchPrompt {
        objective,
        existing_files: &source_context,
    })?;
    let task = ReplacementTask {
        task_id,
        objective: &prompt,
        owned_paths,
        preimages: &preimages,
    };
    let transport: &dyn Transport = match (&selected.transport, request.transport) {
        (_, Some(given)) => given,
        (Some(built), None) => built.as_ref(),
        (None, None) => return fail("No inference transport available"),
    };
    let result = host.generate(&selected.worker, &selected.reviewer, &task, transport)?;
    // Detect any worktree change during inference, before persisting accepted evidence.
    require(
        host.audit(&workspace, &original)? == checkpoint_files,
        "Worktree changed during inference; dispatch remains unresolved",
    )?;
    require(
        result.replacements.iter().all(|item| {
            preimages.get(&item.path).map(String::as_str) == Some(item.expected_sha256.as_str())
        }),
        "Replacement preimage mismatch",
    )?;
    write_review(
        host,
        &root,
        &ReviewContext {
            number,
            claim_hash: &claim_hash,
            reservation_hash: &reservation_hash,
            checkpoint_hash: &checkpoint_hash,
            opened: &opened,
            workspace_name,
            request,
            selected: &selected,
            source_context: &source_context,
        },
        &result,
    )
}

struct ReviewContext<'a> {
    number: usize,
    claim_hash: &'a str,
    reservation_hash: &'a str,
    checkpoint_hash: &'a str,
    opened: &'a Map<String, Value>,
    workspace_name: &'a str,
    request: &'a DispatchRequest<'a>,
    selected: &'a Selected,
    source_context: &'a OrderedMap<String>,
}

fn write_review(
    host: &dyn DispatchHost,
    root: &Path,
    ctx: &ReviewContext<'_>,
    result: &ReplacementReview,
) -> Result<Value, RepairDispatchError> {
    let task_id = ctx.request.task_id;
    // Keep genuine model review separate from bounded deterministic admission policy.
    let unchanged = result
        .replacements
        .iter()
        .all(|item| ctx.source_context.get(&item.path) == Some(&item.content));
    let accepted = result.verdict.as_str() == "accept";
    let no_op_accepted = unchanged && accepted;
    let mut syntax_failures: Vec<String> = Vec::new();
    for item in &result.replacements {
        // Python `str.endswith` is case-sensitive; keep that parity.
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        let is_python = item.path.ends_with(".py");
        if is_python && !host.python_parses(&item.path, &item.content)? {
            syntax_failures.push(format!("Invalid Python syntax in {}", item.path));
        }
    }
    let policy_rejected = no_op_accepted || !syntax_failures.is_empty();
    let effective = if policy_rejected && accepted {
        "changes_required"
    } else {
        result.verdict.as_str()
    };
    let mut findings: Vec<String> = result.findings.clone();
    if no_op_accepted {
        findings.push("No-op replacement: all proposed contents match their preimages".to_owned());
    }
    findings.extend(syntax_failures.iter().cloned());
    let replacements: Vec<Value> = result
        .replacements
        .iter()
        .map(|item| {
            json!({
                "path": item.path,
                "expected_sha256": item.expected_sha256,
                "content": item.content,
                "content_sha256": sha256_hex(item.content.as_bytes()),
            })
        })
        .collect();
    let missing = |name: &str| {
        ctx.opened
            .get(name)
            .cloned()
            .ok_or_else(|| WorkflowError::Message(format!("'{name}'")))
    };
    let evidence = json!({
        "mode": "repair-independent-review",
        "task_id": task_id,
        "number": ctx.number,
        "claim_sha256": ctx.claim_hash,
        "reservation_sha256": ctx.reservation_hash,
        "checkpoint_sha256": ctx.checkpoint_hash,
        "source_failure_sha256": missing("failure_sha256")?,
        "baseline": missing("baseline")?,
        "workspace": ctx.workspace_name,
        "owned_paths": ctx.request.owned_paths,
        "worker_model": ctx.selected.worker.model,
        "reviewer_model": ctx.selected.reviewer.model,
        "verdict": effective,
        "model_verdict": result.verdict.as_str(),
        "policy_rejected_no_op": no_op_accepted,
        "policy_rejected_invalid_syntax": !syntax_failures.is_empty(),
        "findings": findings,
        "replacements": replacements,
        "applied": false,
        "generated_code_executed": false,
        "human_decision_pending": true,
    });
    let review_path = root.join(format!("{task_id}-repair-{}-review.json", ctx.number));
    let digest = write_once(&review_path, &evidence)?;
    let mut output = evidence;
    if let Value::Object(map) = &mut output {
        map.insert("evidence".to_owned(), json!(py_path_str(&review_path)));
        map.insert("evidence_sha256".to_owned(), json!(digest));
    }
    Ok(output)
}

#[cfg(test)]
mod schema_tests {
    #![allow(clippy::unwrap_used)]

    #[test]
    fn replacement_schema_snapshot() {
        let text = crate::util::ordered_json_spaced(&super::replacement_schema()).unwrap();
        assert_eq!(
            text,
            r#"{"type": "object", "properties": {"task_id": {"type": "string"}, "replacements": {"type": "array", "minItems": 1, "maxItems": 16, "items": {"type": "object", "properties": {"path": {"type": "string"}, "expected_sha256": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "expected_sha256", "content"], "additionalProperties": false}}}, "required": ["task_id", "replacements"], "additionalProperties": false}"#
        );
    }
}
