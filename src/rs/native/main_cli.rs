//! Diagnostic invocation of Hermes-native skill backend actions.
//!
//! Backs the `hermes-native` binary.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde_json::Value;

use super::workflows::{assemble, game_plan_file, inspect_run, investigate};
use crate::multiagent::runtime::WorkflowError;
use crate::util::{JsonError, pretty_json_value, sorted_json_spaced_value};

/// Failure of a diagnostic action (Python exit status 2).
#[derive(Debug, thiserror::Error)]
pub enum MainCliError {
    /// Workflow rule violation or evidence failure.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

impl MainCliError {
    /// The Python exception class name reported in the `error` field.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        use std::io::ErrorKind;
        match self {
            Self::Workflow(WorkflowError::Io(error)) => match error.kind() {
                ErrorKind::NotFound => "FileNotFoundError",
                ErrorKind::AlreadyExists => "FileExistsError",
                ErrorKind::PermissionDenied => "PermissionError",
                ErrorKind::IsADirectory => "IsADirectoryError",
                ErrorKind::NotADirectory => "NotADirectoryError",
                _ => "OSError",
            },
            // Python wraps admission decode failures in `WorkflowError(...) from exc`.
            Self::Workflow(WorkflowError::Json(_)) => "JSONDecodeError",
            Self::Workflow(_) | Self::Encode(_) => "WorkflowError",
        }
    }

    /// The line Python prints on standard error (keys already in insertion order): `{"error": kind, "message": text}`.
    ///
    /// # Errors
    /// The message cannot be encoded.
    pub fn report(&self) -> Result<String, JsonError> {
        sorted_json_spaced_value(&serde_json::json!({
            "error": self.kind(),
            "message": self.to_string(),
        }))
    }
}

/// Diagnostic actions.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Action {
    /// Read-only repository facts.
    LetsInvestigate {
        /// Repository.
        #[arg(long)]
        repo: PathBuf,
    },
    /// Validate a plan file.
    GamePlan {
        /// Plan file.
        #[arg(long)]
        plan: PathBuf,
    },
    /// Run the offline deterministic assembly.
    AgentsAssemble {
        /// Repository.
        #[arg(long)]
        repo: PathBuf,
        /// Plan file.
        #[arg(long)]
        plan: PathBuf,
        /// State directory.
        #[arg(long)]
        state: PathBuf,
        /// Required acknowledgement that nothing live runs.
        #[arg(long)]
        confirm_offline: bool,
    },
    /// Inspect a persisted run.
    Inspect {
        /// State directory.
        #[arg(long)]
        state: PathBuf,
        /// Run identifier.
        #[arg(long)]
        run_id: String,
    },
}

/// Command line of `python -m hermes_harness.native`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(
    name = "python -m hermes_harness.native",
    args_override_self = true,
    infer_long_args = true
)]
pub struct Args {
    /// Action to run.
    #[command(subcommand)]
    pub action: Action,
}

fn invoke(args: &Args) -> Result<Value, WorkflowError> {
    match &args.action {
        Action::LetsInvestigate { repo } => investigate(repo),
        Action::GamePlan { plan } => game_plan_file(plan),
        Action::AgentsAssemble {
            repo,
            plan,
            state,
            confirm_offline,
        } => {
            if !confirm_offline {
                return Err(WorkflowError::msg("Explicit --confirm-offline is required"));
            }
            assemble(repo, state, plan)
        }
        Action::Inspect { state, run_id } => inspect_run(state, run_id),
    }
}

/// Run one action and return the text Python prints (`json.dumps(sort_keys=True, indent=2)`).
///
/// # Errors
/// [`MainCliError`]; callers print [`MainCliError::report`] to standard error and exit with 2.
pub fn run(args: &Args) -> Result<String, MainCliError> {
    Ok(pretty_json_value(&invoke(args)?)?)
}
