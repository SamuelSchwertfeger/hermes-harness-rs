//! Small auditable CLI. Ticket state changes are local only.
//!
//! Backs the `hermes-harness` binary.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::builder::PossibleValuesParser;
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::core::ollama::{ENDPOINT, OllamaError, OllamaOptions, text_task};
use crate::core::routing::{Role, RoutingError, select};
use crate::core::state::{Stage, StateError, Ticket, create, load, save};
use crate::multiagent::cli::{Args as MultiagentArgs, CliError as MultiagentError};
use crate::util::ordered_map::OrderedMap;
use crate::util::{JsonError, pretty_json_ordered, py_path_str};

const EVIDENCE_KEYS: [&str; 7] = [
    "facts",
    "plan",
    "commit",
    "verification",
    "review",
    "ci",
    "merged_pr",
];

/// Failure of a top-level command (Python: an uncaught exception, exit status 1).
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Argument parsing failed or help/version was requested (clap prints and picks the status).
    #[error(transparent)]
    Usage(#[from] clap::Error),
    /// Ticket state failure.
    #[error(transparent)]
    State(#[from] StateError),
    /// Route selection refused.
    #[error(transparent)]
    Routing(#[from] RoutingError),
    /// Local model failure.
    #[error(transparent)]
    Ollama(#[from] OllamaError),
    /// `multiagent` subcommand failure.
    #[error(transparent)]
    Multiagent(#[from] MultiagentError),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Python-compatible JSON encoding failure.
    #[error(transparent)]
    Encode(#[from] JsonError),
}

/// Top-level commands (Python `argparse` sub-parsers).
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Action {
    /// Create a ticket.
    New {
        /// Ticket slug.
        ticket: String,
        /// One-line summary.
        summary: String,
    },
    /// Show a ticket.
    Show {
        /// Ticket slug.
        ticket: String,
    },
    /// Write the baton handoff file.
    Baton {
        /// Ticket slug.
        ticket: String,
    },
    /// Advance a ticket to another stage.
    Advance {
        /// Ticket slug.
        ticket: String,
        /// Target stage.
        stage: Stage,
    },
    /// Record a human approval.
    Approve {
        /// Ticket slug.
        ticket: String,
        /// Approval gate.
        #[arg(value_parser = PossibleValuesParser::new(["plan", "merge"]))]
        gate: String,
    },
    /// Record evidence.
    Evidence {
        /// Ticket slug.
        ticket: String,
        /// Evidence key.
        #[arg(value_parser = PossibleValuesParser::new(EVIDENCE_KEYS))]
        key: String,
        /// Evidence value.
        value: String,
    },
    /// Print a route.
    Route {
        /// Agent role.
        role: Role,
    },
    /// Run a prompt on the local scout model.
    LocalScout {
        /// Prompt.
        prompt: String,
    },
}

/// Command line of `hermes-harness`.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(
    name = "hermes-harness",
    about,
    args_override_self = true,
    infer_long_args = true
)]
pub struct Args {
    /// State root directory.
    #[arg(long, default_value = ".harness-runtime")]
    pub state: PathBuf,
    /// Command to run.
    #[command(subcommand)]
    pub action: Action,
}

#[derive(Serialize)]
struct TicketReport<'a> {
    id: &'a str,
    stage: &'static str,
    evidence: &'a OrderedMap<String>,
}

fn baton(root: &Path, ticket: &Ticket) -> Result<PathBuf, CliError> {
    let path = root
        .join("tickets")
        .join(&ticket.id)
        .join("handoffs")
        .join("BATON.md");
    std::fs::write(
        &path,
        format!(
            "# Baton: {}\n\nStage: {}\n\n\
             Read ../state.json, ../facts.md, ../plan.md and briefs before proceeding.\n",
            ticket.id,
            ticket.stage.as_str()
        ),
    )?;
    Ok(path)
}

fn update(root: &Path, action: &Action, ticket: &mut Ticket) -> Result<(), CliError> {
    match action {
        Action::Advance { stage, .. } => ticket.advance(*stage)?,
        Action::Approve { gate, .. } => {
            ticket.approvals.insert(gate.clone(), true);
        }
        Action::Evidence { key, value, .. } => {
            ticket.evidence.insert(key.clone(), value.clone());
        }
        _ => return Ok(()),
    }
    save(root, ticket)?;
    Ok(())
}

fn ticket_command(root: &Path, action: &Action) -> Result<String, CliError> {
    let mut ticket = match action {
        Action::New { ticket, summary } => create(root, ticket, summary)?,
        Action::Show { ticket }
        | Action::Baton { ticket }
        | Action::Advance { ticket, .. }
        | Action::Approve { ticket, .. }
        | Action::Evidence { ticket, .. } => load(root, ticket)?,
        Action::Route { .. } | Action::LocalScout { .. } => return Ok(String::new()),
    };
    if matches!(action, Action::Baton { .. }) {
        return Ok(py_path_str(&baton(root, &ticket)?));
    }
    update(root, action, &mut ticket)?;
    Ok(pretty_json_ordered(&TicketReport {
        id: &ticket.id,
        stage: ticket.stage.as_str(),
        evidence: &ticket.evidence,
    })?)
}

/// Execute one parsed command and return the text Python prints.
///
/// # Errors
/// [`CliError`] for state, routing, model, filesystem or encoding failures.
pub fn run(args: &Args) -> Result<String, CliError> {
    match &args.action {
        Action::Route { role } => Ok(pretty_json_ordered(&select(*role, false)?)?),
        Action::LocalScout { prompt } => {
            let options = OllamaOptions {
                url: ENDPOINT.to_owned(),
                lock_file: args.state.join("gpu.lock"),
            };
            Ok(text_task(prompt, Role::Scout, Some(&options))?)
        }
        action => ticket_command(&args.state, action),
    }
}

/// Parse `argv` (program name first) and run the command; `multiagent` is forwarded to
/// [`crate::multiagent::cli`] when it is the first argument, like the Python entry point.
///
/// # Errors
/// [`CliError::Usage`] for bad arguments, or any failure of the command.
pub fn run_argv<I, T>(argv: I) -> Result<String, CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let argv: Vec<OsString> = argv.into_iter().map(Into::into).collect();
    if argv.get(1).is_some_and(|first| first == "multiagent") {
        let rest = argv.iter().skip(2).cloned();
        let parsed = MultiagentArgs::try_parse_from(
            std::iter::once(OsString::from("multiagent")).chain(rest),
        )?;
        return Ok(crate::multiagent::cli::run(&parsed)?);
    }
    run(&Args::try_parse_from(argv)?)
}
