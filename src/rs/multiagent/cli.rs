//! Explicitly offline multi-task operator commands; no live execution entrypoint.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use super::runtime::{
    Decision, DemoFactory, ExecutionConfig, Node, Plan, Role, RunState, RunSummary, TaskStatus,
    WorkflowError, decide, execute, inspect, ordered_nodes,
};
use crate::core::delegation::create_private_dir;
use crate::util::{JsonError, pretty_json_ordered, random_hex};

/// Failure of an operator command.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Workflow rule violation or evidence failure.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Malformed plan file.
    #[error("{0}")]
    Plan(String),
    /// Git helper failure.
    #[error("{0}")]
    Git(String),
    /// Filesystem failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed JSON plan.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

/// Operator decision choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Choice {
    /// Approve.
    #[value(name = "approve")]
    Approve,
    /// Reject.
    #[value(name = "reject")]
    Reject,
    /// Request changes.
    #[value(name = "request-changes")]
    RequestChanges,
}

impl From<Choice> for Decision {
    fn from(choice: Choice) -> Self {
        match choice {
            Choice::Approve => Self::Approve,
            Choice::Reject => Self::Reject,
            Choice::RequestChanges => Self::RequestChanges,
        }
    }
}

/// Subcommands (Python `argparse` sub-parsers).
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Action {
    /// Run a temporary two-task demonstration.
    Demo,
    /// Validate a JSON plan and print its order.
    Plan {
        /// JSON plan, see docs/multiagent-runtime.md
        file: PathBuf,
    },
    /// Execute a JSON plan offline.
    RunOffline {
        /// JSON plan.
        file: PathBuf,
        /// Source repository.
        #[arg(long)]
        repo: PathBuf,
    },
    /// Show run status.
    Status {
        /// Run identifier.
        run_id: String,
    },
    /// Inspect a run.
    Inspect {
        /// Run identifier.
        run_id: String,
    },
    /// Reconcile a run (read-only).
    Reconcile {
        /// Run identifier.
        run_id: String,
    },
    /// Cancel a run (fail-closed note only).
    Cancel {
        /// Run identifier.
        run_id: String,
    },
    /// Record an operator decision.
    Decision {
        /// Run identifier.
        run_id: String,
        /// Decision.
        #[arg(value_enum)]
        choice: Choice,
    },
    /// Print offline capability facts.
    Doctor,
}

/// Command line of `hermes-harness multiagent`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(name = "hermes-harness multiagent")]
pub struct Args {
    /// State root directory.
    #[arg(long, default_value = ".harness-multiagent")]
    pub state: PathBuf,
    /// Command to run.
    #[command(subcommand)]
    pub action: Action,
}

#[derive(Serialize)]
struct DemoReport {
    mode: &'static str,
    state: RunState,
    tasks: Vec<TaskStatus>,
    decision: Option<Value>,
    note: &'static str,
}

#[derive(Serialize)]
struct PlanReport {
    ordered_nodes: Vec<String>,
    mode: &'static str,
    parallel: bool,
}

#[derive(Serialize)]
struct DoctorReport {
    offline: bool,
    live_gateway: &'static str,
    provider_execution: &'static str,
    concurrency: u32,
    command_isolation: &'static str,
}

#[derive(Serialize)]
struct CancelReport {
    run_id: String,
    tasks: Vec<TaskStatus>,
    state: RunState,
    decision: Option<Value>,
    task_evidence: String,
    cancel_note: &'static str,
}

fn git(repo: &Path, arguments: &[&str]) -> Result<(), CliError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CliError::Git(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn demo_plan() -> Plan {
    Plan::new(
        "demo",
        "Two bounded offline tasks",
        vec![
            Node::new("first", "Create first evidence", &["first.txt"]),
            Node::new("second", "Create second evidence", &["second.txt"])
                .with_dependencies(&["first"]),
        ],
    )
}

fn demo_in(root: &Path) -> Result<RunSummary, CliError> {
    let repo = root.join("source");
    create_private_dir(&repo)?;
    git(&repo, &["init", "-q", "-b", "dev"])?;
    git(
        &repo,
        &[
            "-c",
            "user.name=Offline Demo",
            "-c",
            "user.email=demo@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "baseline",
        ],
    )?;
    let state = root.join("state");
    create_private_dir(&state)?;
    let factory = DemoFactory;
    Ok(execute(
        &state,
        &demo_plan(),
        &ExecutionConfig::new(repo, &factory),
    )?)
}

fn demo() -> Result<String, CliError> {
    let root = std::env::temp_dir().join(format!("hermes-offline-demo-{}", random_hex(4)?));
    create_private_dir(&root)?;
    let result = demo_in(&root);
    let _ = std::fs::remove_dir_all(&root);
    let result = result?;
    Ok(pretty_json_ordered(&DemoReport {
        mode: "offline",
        state: result.state,
        tasks: result.tasks,
        decision: result.decision,
        note: "Temporary demonstration removed; no live provider or merge executed",
    })?)
}

fn plan_text(item: &Value, key: &str) -> Result<String, CliError> {
    item.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| CliError::Plan(format!("Plan field {key} must be a string")))
}

fn plan_strings(item: &Value, key: &str) -> Result<Vec<String>, CliError> {
    let Some(value) = item.get(key) else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .map(|row| row.as_str().map(str::to_owned))
                .collect()
        })
        .ok_or_else(|| CliError::Plan(format!("Plan field {key} must be an array of strings")))
}

fn load_plan(path: &Path) -> Result<Plan, CliError> {
    let document: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let rows = document
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| CliError::Plan("Plan field nodes must be an array".to_owned()))?;
    let mut nodes = Vec::new();
    for item in rows {
        nodes.push(Node {
            id: plan_text(item, "id")?,
            objective: plan_text(item, "objective")?,
            owned_paths: plan_strings(item, "owned_paths")?,
            dependencies: plan_strings(item, "dependencies")?,
            role: Role::Coding,
        });
    }
    let primary_identity = match document.get("primary_identity") {
        Some(_) => plan_text(&document, "primary_identity")?,
        None => "offline-primary".to_owned(),
    };
    Ok(Plan {
        id: plan_text(&document, "id")?,
        objective: plan_text(&document, "objective")?,
        nodes,
        primary_identity,
    })
}

fn run_plan(args: &Args, file: &Path, repo: Option<&Path>) -> Result<String, CliError> {
    let plan = load_plan(file)?;
    let nodes = ordered_nodes(&plan)?;
    let Some(repo) = repo else {
        return Ok(pretty_json_ordered(&PlanReport {
            ordered_nodes: nodes.into_iter().map(|node| node.id).collect(),
            mode: "offline",
            parallel: false,
        })?);
    };
    std::fs::create_dir_all(&args.state)?;
    let factory = DemoFactory;
    let result = execute(&args.state, &plan, &ExecutionConfig::new(repo, &factory))?;
    Ok(pretty_json_ordered(&result)?)
}

fn cancel(summary: RunSummary) -> CancelReport {
    CancelReport {
        run_id: summary.run_id,
        tasks: summary.tasks,
        state: summary.state,
        decision: summary.decision,
        task_evidence: summary.task_evidence,
        cancel_note: "No running background process exists; stop is fail-closed. \
                      Existing evidence retained; no resubmission.",
    }
}

/// Execute one parsed command and return its JSON output (Python `json.dumps(indent=2)`).
///
/// # Errors
/// [`CliError`] for workflow, plan-file, Git or I/O failures.
pub fn run(args: &Args) -> Result<String, CliError> {
    match &args.action {
        Action::Demo => demo(),
        Action::Doctor => Ok(pretty_json_ordered(&DoctorReport {
            offline: true,
            live_gateway: "unqualified",
            provider_execution: "disabled",
            concurrency: 1,
            command_isolation: "not provided by offline doubles",
        })?),
        Action::Plan { file } => run_plan(args, file, None),
        Action::RunOffline { file, repo } => run_plan(args, file, Some(repo)),
        Action::Decision { run_id, choice } => Ok(pretty_json_ordered(&decide(
            &args.state,
            run_id,
            (*choice).into(),
        )?)?),
        Action::Cancel { run_id } => {
            Ok(pretty_json_ordered(&cancel(inspect(&args.state, run_id)?))?)
        }
        Action::Status { run_id } | Action::Inspect { run_id } | Action::Reconcile { run_id } => {
            Ok(pretty_json_ordered(&inspect(&args.state, run_id)?)?)
        }
    }
}
