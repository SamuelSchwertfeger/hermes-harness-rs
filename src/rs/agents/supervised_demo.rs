//! Opt-in OFFLINE demonstration with disposable Git and no network gateway.

use crate::util::PathLexical as _;
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use serde::Serialize;

use super::supervised_agents::SupervisedPolicy;
use super::supervised_execution::{
    ExecutionBlockedError, ScriptedExecution, execute_scripted_once,
};
use super::supervised_reviewer_dispatch::{
    ReviewerDispatchBlockedError, RuntimeBinding, RuntimeQualifier,
};
use crate::core::delegation::{Brief, DelegationError, Finding, OfflineAgent, create_private_dir};
use crate::gateway::gateway_identity::Submission;
use crate::gateway::scripted_supervised_gateway::ScriptedSupervisedGateway;
use crate::git::worktree_delegation::TaskWorkspace;
use crate::util::{JsonError, pretty_json_ordered, py_path_str, resolve_lenient, uuid4_hex};

/// Failure of the demonstration.
#[derive(Debug, thiserror::Error)]
pub enum DemoError {
    /// Refused argument (Python `parser.error`).
    #[error("{0}")]
    Usage(String),
    /// The scripted flow was blocked or failed.
    #[error(transparent)]
    Execution(#[from] ExecutionBlockedError),
    /// Filesystem or process failure.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

/// Not a live sandbox attestation; only valid with built-in scripted gateway.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScriptedOnlyQualifier;

impl RuntimeQualifier for ScriptedOnlyQualifier {
    fn require_qualified(
        &self,
        binding: &RuntimeBinding,
    ) -> Result<(), ReviewerDispatchBlockedError> {
        if !binding.workspace.is_dir() || binding.workspace.is_symlink_lexical() {
            return Err(ReviewerDispatchBlockedError::Blocked(
                "Scripted workspace is not a real directory".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Deterministic worker double that writes one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoWorker {
    /// Worktree the double writes into.
    pub workspace: PathBuf,
}

impl OfflineAgent for DemoWorker {
    fn run(&mut self, brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        if prior.is_some() {
            return Err(DelegationError::Message(
                "Worker cannot receive prior evidence".to_owned(),
            ));
        }
        std::fs::write(
            self.workspace.join("demo-result.txt"),
            "offline scripted worker output\n",
        )?;
        Ok(Finding {
            agent_id: brief.worker_id.clone(),
            text: "Demo worker completed".to_owned(),
        })
    }
}

/// Separate non-provider reviewer double.
#[derive(Debug, Clone, Copy, Default)]
pub struct DemoReviewer;

impl OfflineAgent for DemoReviewer {
    fn run(&mut self, brief: &Brief, prior: Option<&Finding>) -> Result<Finding, DelegationError> {
        match prior {
            Some(finding) if finding.agent_id == brief.worker_id => Ok(Finding {
                agent_id: brief.reviewer_id.clone(),
                text: "Scripted review only: request human approval".to_owned(),
            }),
            _ => Err(DelegationError::Message(
                "Missing worker evidence".to_owned(),
            )),
        }
    }
}

fn git(repo: &Path, args: &[&str]) -> io::Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

/// Command-line arguments of the demonstration.
#[derive(Debug, Parser)]
#[command(
    about = "OFFLINE ONLY supervised gateway demonstration",
    args_override_self = true,
    infer_long_args = true
)]
pub struct Args {
    /// New, nonexistent evidence root.
    #[arg(long)]
    pub root: PathBuf,
}

#[derive(Serialize)]
struct Report {
    mode: &'static str,
    worker_admission: String,
    reviewer_admission: String,
    audit_sha256: String,
    approval_request: String,
    approved: bool,
}

fn init_repo(repo: &Path) -> io::Result<()> {
    std::fs::create_dir(repo)?;
    git(repo, &["init"])?;
    git(repo, &["config", "user.email", "offline@example.invalid"])?;
    git(repo, &["config", "user.name", "Offline qualification"])?;
    std::fs::write(repo.join("README.md"), "disposable baseline\n")?;
    git(repo, &["add", "README.md"])?;
    git(repo, &["commit", "-m", "baseline"])
}

/// Create ONE disposable repository, run the scripted flow, and return the JSON report.
///
/// # Errors
/// Task preparation, scripted execution, or evidence persistence fails.
pub fn run(args: &Args) -> Result<String, DemoError> {
    let root = resolve_lenient(&args.root);
    if root.exists() {
        return Err(DemoError::Usage(
            "Root must not exist; old evidence is never overwritten".to_owned(),
        ));
    }
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent)?;
    }
    create_private_dir(&root)?;
    let repo = root.join("repo");
    init_repo(&repo)?;
    let state = root.join("state");
    std::fs::create_dir(&state)?;
    let workspace = root.join("workspace");
    let brief = Brief {
        task_id: "scripted-demo".to_owned(),
        objective: "Scripted worker reviewer integration".to_owned(),
        owned_paths: vec!["demo-result.txt".to_owned()],
        worker_id: "worker".to_owned(),
        reviewer_id: "reviewer".to_owned(),
    };
    let gateway = RefCell::new(ScriptedSupervisedGateway::default());
    let worker = RefCell::new(DemoWorker {
        workspace: workspace.clone(),
    });
    let reviewer = RefCell::new(DemoReviewer);
    let config = ScriptedExecution {
        policy: SupervisedPolicy::new(&brief.task_id, &workspace),
        task: TaskWorkspace {
            state_root: state,
            workspace,
            brief,
        },
        source_repo: repo,
        worker_submission: Submission::new("worker-scripted", &uuid4_hex()?),
        reviewer_submission: Submission::new("reviewer-scripted", &uuid4_hex()?),
        ledger_path: root.join("admissions.sqlite"),
        rpc: &gateway,
        qualifier: &ScriptedOnlyQualifier,
        worker: &worker,
        reviewer: &reviewer,
    };
    let result = execute_scripted_once(&config)?;
    Ok(pretty_json_ordered(&Report {
        mode: "OFFLINE SCRIPTED (NO HERMES)",
        worker_admission: result.worker_admission_id,
        reviewer_admission: result.reviewer_admission_id,
        audit_sha256: result.audit_sha256,
        approval_request: py_path_str(&result.approval_request),
        approved: false,
    })?)
}
