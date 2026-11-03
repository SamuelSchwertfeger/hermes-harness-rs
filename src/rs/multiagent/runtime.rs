//! Durable multi-task orchestration over canonical audited offline delegation.
//!
//! All injected agents are trusted deterministic doubles. This is NOT an OS sandbox,
//! and no provider, gateway, network, Git merge, or external tool is invoked by agents.

use crate::util::PathLexical as _;
use crate::util::{is_py_blank, is_slug_id};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::Serialize;
use serde_json::{Value, json};

use super::cyber::{CyberAuthorizationError, CyberGrant, validate_cyber_node};
use crate::core::delegation::{
    Brief, DelegationError, Finding, OfflineAgent, create_private_dir, write_once_value,
};
use crate::git::worktree_delegation::{TaskWorkspace, WorktreeDelegationError};
use crate::supervised::offline_orchestrator::{OfflineAgents, run_offline_task};
use crate::util::{JsonError, py_path_str, sha256_hex};

/// Maximum number of plan nodes.
pub const MAX_NODES: usize = 32;
/// Maximum number of owned paths per node.
pub const MAX_PATHS: usize = 32;
/// Maximum length of the combined `<plan>-<node>` task identifier.
pub const MAX_TASK_ID_LENGTH: usize = 64;

/// Invalid plan or fenced workflow; operator inspection required.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkflowError {
    /// Rule violation (Python `WorkflowError`).
    #[error("{0}")]
    Message(String),
    /// JSON admission failure with the original decoder error.
    #[error("{message}")]
    AdmissionJson {
        /// Parity-preserving refusal text.
        message: String,
        /// Underlying JSON decoder failure.
        #[source]
        source: serde_json::Error,
    },
    /// Cyber Warrior authorization failure.
    #[error(transparent)]
    Cyber(#[from] CyberAuthorizationError),
    /// Failure of the audited worktree delegation.
    #[error(transparent)]
    Worktree(#[from] WorktreeDelegationError),
    /// Failure of the canonical delegation layer.
    #[error(transparent)]
    Delegation(#[from] DelegationError),
    /// Filesystem failure; `AlreadyExists` mirrors Python `FileExistsError`.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed JSON record.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

impl WorkflowError {
    /// Build a rule-violation error.
    #[must_use]
    pub fn msg(message: &str) -> Self {
        Self::Message(message.to_owned())
    }

    /// Whether this is an exclusive-create collision (Python `FileExistsError`).
    #[must_use]
    pub fn is_already_exists(&self) -> bool {
        matches!(self, Self::Io(error) if error.kind() == std::io::ErrorKind::AlreadyExists)
    }
}

pub(crate) fn fail<T>(message: &str) -> Result<T, WorkflowError> {
    Err(WorkflowError::msg(message))
}

crate::util::wire_enum! {
    /// Worker role of a node.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
    pub enum Role {
        /// Ordinary coding worker.
        #[default]
        Coding => "coding",
        /// Security specialist; requires an operator grant.
        CyberWarrior => "cyber-warrior",
    }

    /// The Python role string.
    as_str;
    from_str(value) -> WorkflowError { WorkflowError::msg("Unknown worker role") }
}

crate::util::wire_enum! {
    /// Operator decision on a completed run.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
    pub enum Decision {
        /// `approve`
        Approve => "approve",
        /// `reject`
        Reject => "reject",
        /// `request-changes`
        RequestChanges => "request-changes",
    }

    /// The Python decision string.
    as_str;
    from_str(value) -> WorkflowError { WorkflowError::msg("Invalid human decision") }
}

crate::util::wire_enum! {
    /// State of one node derived from its receipt and fence files.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
    pub enum TaskState {
        /// Approval-ready receipt exists.
        Completed => "completed",
        /// A receipt exists but is not approval-ready.
        Failed => "failed",
        /// A dispatch fence exists without a receipt.
        Unknown => "unknown",
        /// Nothing recorded yet.
        Pending => "pending",
    }

    /// The Python state string.
    as_str;
}

crate::util::wire_enum! {
    /// State of a persisted run.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
    pub enum RunState {
        /// A decision record exists and awaits the operator.
        PendingHumanDecision => "pending_human_decision",
        /// No decision record: fenced or incomplete.
        FencedOrIncomplete => "fenced_or_incomplete",
    }

    /// The Python state string.
    as_str;
}

/// Bounded task node with exclusive ownership and dependencies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Node {
    /// Node identifier.
    pub id: String,
    /// Objective text.
    pub objective: String,
    /// Exclusively owned relative paths.
    pub owned_paths: Vec<String>,
    /// Node IDs that must complete first.
    pub dependencies: Vec<String>,
    /// Worker role.
    pub role: Role,
}

impl Node {
    /// Node with no dependencies and the default `coding` role.
    #[must_use]
    pub fn new(id: &str, objective: &str, owned_paths: &[&str]) -> Self {
        Self {
            id: id.to_owned(),
            objective: objective.to_owned(),
            owned_paths: owned_paths.iter().map(|path| (*path).to_owned()).collect(),
            dependencies: Vec::new(),
            role: Role::Coding,
        }
    }

    /// Replace the dependencies.
    #[must_use]
    pub fn with_dependencies(mut self, dependencies: &[&str]) -> Self {
        self.dependencies = dependencies.iter().map(|item| (*item).to_owned()).collect();
        self
    }

    /// Replace the role.
    #[must_use]
    pub const fn with_role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }
}

/// Primary coordinator output, validated before any worktree operation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Plan {
    /// Plan (run) identifier.
    pub id: String,
    /// Plan objective.
    pub objective: String,
    /// Task nodes.
    pub nodes: Vec<Node>,
    /// Identity of the primary coordinator.
    pub primary_identity: String,
}

impl Plan {
    /// Plan with the default `offline-primary` identity.
    #[must_use]
    pub fn new(id: &str, objective: &str, nodes: Vec<Node>) -> Self {
        Self {
            id: id.to_owned(),
            objective: objective.to_owned(),
            nodes,
            primary_identity: "offline-primary".to_owned(),
        }
    }
}

/// Trusted offline planner contract; never given execution capabilities.
pub trait Primary {
    /// Produce a plan for a request.
    ///
    /// # Errors
    /// Planner failure.
    fn plan(&self, request: &str) -> Result<Plan, WorkflowError>;
}

/// Shared handle to an offline agent.
///
/// Seam: `Rc` pointer identity reproduces Python's `worker is reviewer` independence check;
/// `RefCell` gives the `&mut self` agent a single-threaded shared handle.
pub type SharedAgent = Rc<RefCell<dyn OfflineAgent>>;

/// Create an independent offline double for a bound task workspace.
pub trait WorkerFactory {
    /// Worker for a node working in `workspace`.
    ///
    /// # Errors
    /// Factory failure; the dispatch fence stays in place.
    fn worker(&self, node: &Node, workspace: &Path) -> Result<SharedAgent, WorkflowError>;

    /// Reviewer for a node.
    ///
    /// # Errors
    /// Factory failure; the dispatch fence stays in place.
    fn reviewer(&self, node: &Node) -> Result<SharedAgent, WorkflowError>;
}

/// Offline execution dependencies supplied by the operator.
#[derive(Clone)]
pub struct ExecutionConfig<'a> {
    /// Source repository.
    pub repo: PathBuf,
    /// Agent factory.
    pub factory: &'a dyn WorkerFactory,
    /// Operator grant required for `cyber-warrior` nodes.
    pub cyber_grant: Option<CyberGrant>,
}

impl<'a> ExecutionConfig<'a> {
    /// Config without a Cyber Warrior grant.
    #[must_use]
    pub fn new(repo: impl Into<PathBuf>, factory: &'a dyn WorkerFactory) -> Self {
        Self {
            repo: repo.into(),
            factory,
            cyber_grant: None,
        }
    }
}

impl fmt::Debug for ExecutionConfig<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionConfig")
            .field("repo", &self.repo)
            .field("cyber_grant", &self.cyber_grant)
            .finish_non_exhaustive()
    }
}

/// Per-node state in a run summary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct TaskStatus {
    /// Node identifier.
    pub node: String,
    /// Node state.
    pub state: TaskState,
}

/// Read-only run summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunSummary {
    /// Run identifier.
    pub run_id: String,
    /// Per-node states in persisted plan order.
    pub tasks: Vec<TaskStatus>,
    /// Run state.
    pub state: RunState,
    /// Persisted decision record, if any.
    pub decision: Option<Value>,
    /// Directory holding per-task evidence.
    pub task_evidence: String,
}

/// Outcome of an operator decision; never applies changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionResult {
    /// Run identifier.
    pub run_id: String,
    /// The recorded action.
    pub decision: Decision,
    /// Always `false`.
    pub applied: bool,
}

fn validate_path(path: &str, owners: &mut Vec<String>) -> Result<(), WorkflowError> {
    let unsafe_path = path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
        || path.split('/').next() == Some(".git");
    if unsafe_path {
        return fail("Unsafe ownership path");
    }
    let overlaps = owners.iter().any(|other| {
        path == other
            || path
                .strip_prefix(other.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
            || other
                .strip_prefix(path)
                .is_some_and(|rest| rest.starts_with('/'))
    });
    if overlaps {
        return fail("Overlapping ownership");
    }
    owners.push(path.to_owned());
    Ok(())
}

fn validate_node(
    node: &Node,
    ids: &BTreeSet<&str>,
    owners: &mut Vec<String>,
) -> Result<(), WorkflowError> {
    if !is_slug_id(&node.id) || is_py_blank(&node.objective) {
        return fail("Invalid node");
    }
    if node.owned_paths.is_empty() || node.owned_paths.len() > MAX_PATHS {
        return fail("Invalid ownership cardinality");
    }
    for path in &node.owned_paths {
        validate_path(path, owners)?;
    }
    let unique: BTreeSet<&String> = node.dependencies.iter().collect();
    if node.dependencies.contains(&node.id) || unique.len() != node.dependencies.len() {
        return fail("Invalid dependencies");
    }
    if node
        .dependencies
        .iter()
        .any(|dep| !ids.contains(dep.as_str()))
    {
        return fail("Missing dependency");
    }
    Ok(())
}

fn topological_order(nodes: &[Node]) -> Result<Vec<Node>, WorkflowError> {
    let mut done: BTreeSet<&str> = BTreeSet::new();
    let mut result: Vec<Node> = Vec::new();
    while result.len() < nodes.len() {
        let ready = nodes
            .iter()
            .filter(|node| {
                !done.contains(node.id.as_str())
                    && node
                        .dependencies
                        .iter()
                        .all(|dep| done.contains(dep.as_str()))
            })
            .min_by(|a, b| a.id.cmp(&b.id));
        let Some(next) = ready else {
            return fail("Cyclic dependencies");
        };
        done.insert(next.id.as_str());
        result.push(next.clone());
    }
    Ok(result)
}

/// Validate DAG, IDs and disjoint file ownership, then stable topological sort.
///
/// # Errors
/// [`WorkflowError::Message`] for an invalid identity, node count, ownership, dependency
/// or cycle.
pub fn ordered_nodes(plan: &Plan) -> Result<Vec<Node>, WorkflowError> {
    if !is_slug_id(&plan.id) || is_py_blank(&plan.objective) {
        return fail("Invalid plan identity or objective");
    }
    if plan.nodes.is_empty() || plan.nodes.len() > MAX_NODES {
        return fail("Plan must contain between 1 and 32 nodes");
    }
    let ids: BTreeSet<&str> = plan.nodes.iter().map(|node| node.id.as_str()).collect();
    if ids.len() != plan.nodes.len() {
        return fail("Duplicate node ID");
    }
    let mut owners = Vec::new();
    for node in &plan.nodes {
        validate_node(node, &ids, &mut owners)?;
    }
    topological_order(&plan.nodes)
}

/// Exclusive durable write of canonical JSON plus newline.
fn write_new(path: &Path, value: &Value) -> Result<(), WorkflowError> {
    write_once_value(path, value).map_err(|error| match error {
        DelegationError::Io(io) => WorkflowError::Io(io),
        other => WorkflowError::Delegation(other),
    })
}

fn record(path: &Path) -> Result<Value, WorkflowError> {
    if path.is_symlink_lexical() {
        return fail("Symlink record rejected");
    }
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if !value.is_object() {
        return fail("Invalid record");
    }
    Ok(value)
}

/// Per-node state derived from receipt and fence files.
///
/// # Errors
/// Unreadable, symlinked or malformed receipt.
fn task_status(directory: &Path, name: &str) -> Result<TaskStatus, WorkflowError> {
    let receipt = directory.join(format!("{name}.receipt.json"));
    let fence = directory.join(format!("{name}.dispatch-fence.json"));
    let state = if receipt.is_file() {
        let value = record(&receipt)?;
        if value.get("state").and_then(Value::as_str) == Some("awaiting_approval") {
            TaskState::Completed
        } else {
            TaskState::Failed
        }
    } else if fence.exists() {
        TaskState::Unknown
    } else {
        TaskState::Pending
    };
    Ok(TaskStatus {
        node: name.to_owned(),
        state,
    })
}

fn persisted_tasks(directory: &Path, nodes: &[Value]) -> Result<Vec<TaskStatus>, WorkflowError> {
    nodes
        .iter()
        .map(|node| match node.get("id").and_then(Value::as_str) {
            Some(id) if node.is_object() => task_status(directory, id),
            _ => fail("Invalid persisted plan"),
        })
        .collect()
}

/// Read-only, restart-safe inspection; never redispatch uncertain work.
///
/// # Errors
/// Invalid run ID, or a missing, symlinked or malformed persisted record.
pub fn inspect(root: &Path, run_id: &str) -> Result<RunSummary, WorkflowError> {
    if !is_slug_id(run_id) {
        return fail("Invalid run ID");
    }
    let directory = root.join(run_id);
    let plan = record(&directory.join("plan.json"))?;
    let Some(Value::Array(nodes)) = plan.get("nodes") else {
        return fail("Invalid persisted plan");
    };
    let tasks = persisted_tasks(&directory, nodes)?;
    let decision_path = directory.join("decision.json");
    let decision = if decision_path.exists() {
        Some(record(&decision_path)?)
    } else {
        None
    };
    let state = if decision.is_some() {
        RunState::PendingHumanDecision
    } else {
        RunState::FencedOrIncomplete
    };
    Ok(RunSummary {
        run_id: run_id.to_owned(),
        tasks,
        state,
        decision,
        task_evidence: py_path_str(&directory.join("tasks")),
    })
}

/// Lifetime-erasing forwarder so shared agents fit the orchestrator's borrowed cells.
struct Forward(SharedAgent);

impl OfflineAgent for Forward {
    fn run(&mut self, brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        self.0.borrow_mut().run(brief, prior)
    }
}

fn dispatch_node(
    directory: &Path,
    plan_id: &str,
    execution: &ExecutionConfig<'_>,
    node: &Node,
) -> Result<(), WorkflowError> {
    let task_id = format!("{plan_id}-{}", node.id);
    if task_id.len() > MAX_TASK_ID_LENGTH {
        return fail("Combined task ID too long");
    }
    let brief = Brief {
        task_id: task_id.clone(),
        objective: node.objective.clone(),
        owned_paths: node.owned_paths.clone(),
        worker_id: format!("worker-{}", node.id),
        reviewer_id: format!("reviewer-{}", node.id),
    };
    let workspace = directory.join("workspaces").join(&node.id);
    let task = TaskWorkspace {
        state_root: directory.join("tasks"),
        workspace: workspace.clone(),
        brief: brief.clone(),
    };
    write_new(
        &directory.join(format!("{}.dispatch-fence.json", node.id)),
        &json!({
            "task_id": task_id,
            "role": node.role.as_str(),
            "objective_sha256": sha256_hex(node.objective.as_bytes()),
            "state": "uncertain_until_verified",
        }),
    )?;
    let worker = execution.factory.worker(node, &workspace)?;
    let reviewer = execution.factory.reviewer(node)?;
    if std::ptr::addr_eq(Rc::as_ptr(&worker), Rc::as_ptr(&reviewer)) {
        return fail("Worker and reviewer must be independent");
    }
    let (worker_cell, reviewer_cell) = (
        RefCell::new(Forward(worker)),
        RefCell::new(Forward(reviewer)),
    );
    let agents = OfflineAgents::new(&worker_cell, &reviewer_cell);
    let result = run_offline_task(&task, &execution.repo, &agents)?;
    let digest = |finding: &Option<Finding>| {
        finding
            .as_ref()
            .map_or(Value::Null, |item| json!(sha256_hex(item.text.as_bytes())))
    };
    let approval = result
        .approval_request
        .as_ref()
        .map_or_else(|| "None".to_owned(), |path| path.display().to_string());
    write_new(
        &directory.join(format!("{}.receipt.json", node.id)),
        &json!({
            "task_id": task_id,
            "state": result.state.as_str(),
            "reviewer_id": brief.reviewer_id,
            "worker_sha256": digest(&result.worker),
            "review_sha256": digest(&result.reviewer),
            "approval_request": approval,
        }),
    )
}

/// Run one fresh serial offline plan, fencing every uncertain dispatch.
///
/// # Errors
/// Invalid plan, missing Cyber Warrior grant, existing run directory
/// ([`WorkflowError::is_already_exists`]), factory or delegation failure, or a node whose
/// outcome is not approval-ready.
pub fn execute(
    root: &Path,
    plan: &Plan,
    execution: &ExecutionConfig<'_>,
) -> Result<RunSummary, WorkflowError> {
    let ordered = ordered_nodes(plan)?;
    for node in ordered
        .iter()
        .filter(|node| node.role == Role::CyberWarrior)
    {
        validate_cyber_node(
            &node.id,
            &node.owned_paths,
            &execution.repo,
            execution.cyber_grant.as_ref(),
        )?;
    }
    if !root.is_dir() || root.is_symlink_lexical() {
        return fail("State root must be a real existing directory");
    }
    let directory = root.join(&plan.id);
    create_private_dir(&directory)?;
    for folder in ["tasks", "workspaces"] {
        create_private_dir(&directory.join(folder))?;
    }
    write_new(&directory.join("plan.json"), &serde_json::to_value(plan)?)?;
    for node in &ordered {
        for dependency in &node.dependencies {
            if task_status(&directory, dependency)?.state != TaskState::Completed {
                return fail("Cannot dispatch node after unsuccessful dependency");
            }
        }
        dispatch_node(&directory, &plan.id, execution, node)?;
        if task_status(&directory, &node.id)?.state != TaskState::Completed {
            return fail("Node outcome is not eligible for downstream dispatch");
        }
    }
    write_new(
        &directory.join("decision.json"),
        &json!({"state": "pending", "approved": false, "integrated": false, "run_id": plan.id}),
    )?;
    inspect(root, &plan.id)
}

/// Write an immutable operator decision, without applying changes.
///
/// # Errors
/// Incomplete or uncertain run, or a decision that already exists.
pub fn decide(
    root: &Path,
    run_id: &str,
    action: Decision,
) -> Result<DecisionResult, WorkflowError> {
    let summary = inspect(root, run_id)?;
    if summary.state != RunState::PendingHumanDecision
        || summary
            .tasks
            .iter()
            .any(|row| row.state != TaskState::Completed)
    {
        return fail("Cannot decide on incomplete or uncertain run");
    }
    write_new(
        &root.join(run_id).join("human-decision.json"),
        &json!({"decision": action.as_str(), "does_not_authorize_merge_or_push": true}),
    )?;
    Ok(DecisionResult {
        run_id: run_id.to_owned(),
        decision: action,
        applied: false,
    })
}

/// Trusted offline test double, not a sandboxed agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoWorker {
    /// Task workspace.
    pub workspace: PathBuf,
    /// Relative path to create.
    pub target: String,
    /// Node identifier.
    pub node_id: String,
}

impl OfflineAgent for DemoWorker {
    fn run(&mut self, brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        if prior.is_some() {
            return Err(DelegationError::Agent(
                "Worker received reviewer context".to_owned(),
            ));
        }
        let path = self.workspace.join(&self.target);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, format!("offline evidence for {}\n", self.node_id))?;
        Ok(Finding {
            agent_id: brief.worker_id.clone(),
            text: format!("Created {} in disposable worktree", self.target),
        })
    }
}

/// Trusted offline reviewer double.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DemoReviewer;

impl OfflineAgent for DemoReviewer {
    fn run(&mut self, brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        if prior.is_none() {
            return Err(DelegationError::Agent(
                "Reviewer missing worker evidence".to_owned(),
            ));
        }
        Ok(Finding {
            agent_id: brief.reviewer_id.clone(),
            text: "Offline reviewer received audited worker evidence".to_owned(),
        })
    }
}

/// Factory producing [`DemoWorker`] and [`DemoReviewer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DemoFactory;

impl WorkerFactory for DemoFactory {
    fn worker(&self, node: &Node, workspace: &Path) -> Result<SharedAgent, WorkflowError> {
        let target = node.owned_paths.first().cloned().unwrap_or_default();
        Ok(Rc::new(RefCell::new(DemoWorker {
            workspace: workspace.to_path_buf(),
            target,
            node_id: node.id.clone(),
        })))
    }

    fn reviewer(&self, _node: &Node) -> Result<SharedAgent, WorkflowError> {
        Ok(Rc::new(RefCell::new(DemoReviewer)))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        missing_docs,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::pedantic,
        clippy::nursery
    )]
    //! Fail-closed outcome and dependency state tests for the multiagent runner.

    use super::{TaskState, task_status};
    use serde_json::json;

    #[test]
    fn receipt_only_counts_as_completed_on_approval_ready() {
        let tmp = tempfile::tempdir().unwrap();
        let receipt = tmp.path().join("step.receipt.json");
        for (state, expected) in [
            ("awaiting_approval", TaskState::Completed),
            ("manual_intervention", TaskState::Failed),
            ("dispatch_uncertain", TaskState::Failed),
        ] {
            std::fs::write(&receipt, json!({"state": state}).to_string()).unwrap();
            assert_eq!(task_status(tmp.path(), "step").unwrap().state, expected);
        }
    }

    #[test]
    fn fence_without_receipt_is_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            task_status(tmp.path(), "step").unwrap().state,
            TaskState::Pending
        );
        std::fs::write(tmp.path().join("step.dispatch-fence.json"), "{}").unwrap();
        assert_eq!(
            task_status(tmp.path(), "step").unwrap().state,
            TaskState::Unknown
        );
    }
}
