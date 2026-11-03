//! Internal operator probes; explicit endpoints, injected RPCs, and no autostart.

pub mod disposable_admission;
pub mod functional_fixture;
pub mod live_admission;
pub mod ollama_smoke;
pub mod reconcile_terminal;
pub mod unknown_recovery;
pub mod validate_existing;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::gateway::gateway_readonly::{GatewayRpc, GatewayRpcError};
use crate::gateway::gateway_session_creation::{RpcFailure, SessionRpc};

/// Failure with a bounded Python-compatible category and a local refusal message.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ProbeError {
    /// Sanitized error category printed by the CLI.
    pub category: &'static str,
    /// Refusal reason, available to callers without exposing RPC details on stdout.
    pub message: String,
}

impl ProbeError {
    pub(crate) fn value(message: &str) -> Self {
        Self {
            category: "ValueError",
            message: message.into(),
        }
    }

    pub(crate) fn runtime(error: impl std::fmt::Display) -> Self {
        Self {
            category: "RuntimeError",
            message: error.to_string(),
        }
    }
}

impl From<std::io::Error> for ProbeError {
    fn from(error: std::io::Error) -> Self {
        Self {
            category: "OSError",
            message: error.to_string(),
        }
    }
}

/// A connector must attach once to the supplied endpoint, never discover or start.
pub trait Connector {
    /// Already authenticated RPC implementation.
    type Rpc: GatewayRpc;
    /// Connect to exactly this operator-selected endpoint.
    fn connect(&mut self, endpoint: &Path) -> Result<Self::Rpc, ProbeError>;
}

/// Production connector. No Hermes wire protocol is defined in this repository.
pub struct ProductionConnector;

/// Unavailable production RPC; no method can send a request.
pub struct UnavailableRpc;

/// Fail-closed diagnostic shared by Unix and non-Unix builds.
pub const CLIENT_UNAVAILABLE: &str =
    "Hermes gateway client unavailable in Rust build; use the Python probe";

impl Connector for ProductionConnector {
    type Rpc = UnavailableRpc;

    fn connect(&mut self, _endpoint: &Path) -> Result<Self::Rpc, ProbeError> {
        Err(ProbeError::runtime(CLIENT_UNAVAILABLE))
    }
}

impl GatewayRpc for UnavailableRpc {
    fn rpc(
        &mut self,
        _method: &str,
        _params: &BTreeMap<String, Value>,
    ) -> Result<Value, GatewayRpcError> {
        Err(GatewayRpcError::Value(CLIENT_UNAVAILABLE.into()))
    }
}

pub(crate) struct SessionAdapter<'a, R>(pub &'a mut R);

impl<R: GatewayRpc> SessionRpc for SessionAdapter<'_, R> {
    fn rpc(
        &mut self,
        method: &str,
        params: &serde_json::Map<String, Value>,
    ) -> Result<Value, RpcFailure> {
        self.0
            .rpc(
                method,
                &params.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            )
            .map_err(|error| Box::new(error) as RpcFailure)
    }
}

/// Parsed operator options. OS-native path values are preserved.
#[derive(Debug, Default)]
pub struct Args {
    values: BTreeMap<String, OsString>,
}

impl Args {
    pub(crate) fn has(&self, name: &str) -> bool {
        self.values.contains_key(name)
    }
    pub(crate) fn text(&self, name: &str) -> &str {
        self.values.get(name).and_then(|v| v.to_str()).unwrap_or("")
    }
    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.values
            .get(name)
            .map_or_else(PathBuf::new, PathBuf::from)
    }
    pub(crate) fn connect<C: Connector>(&self, connector: &mut C) -> Result<C::Rpc, ProbeError> {
        let endpoint = self.path("endpoint");
        if endpoint.as_os_str().is_empty() {
            return Err(ProbeError::value(
                "--endpoint is required for gateway access",
            ));
        }
        connector.connect(&endpoint)
    }
}

const PATH_OPTIONS: &[&str] = &["endpoint", "profile-home", "creation-record", "ledger"];

struct Options {
    required: &'static [&'static str],
    values: &'static [&'static str],
    flags: &'static [&'static str],
    about: &'static str,
}

fn options(command: &str) -> Option<Options> {
    Some(match command {
        "disposable-admission" => Options {
            required: &[
                "profile-home",
                "creation-record",
                "ledger",
                "create-request-id",
                "submission-id",
            ],
            values: &["confirm-create-request-id"],
            flags: &["execute", "confirm-disposable"],
            about: "Offline by default. --execute creates one disposable session and submits once; requires exact --confirm-create-request-id and --confirm-disposable.",
        },
        "live-admission" => Options {
            required: &["profile-home", "session-id", "ledger", "submission-id"],
            values: &["confirm-session-id"],
            flags: &["submit", "confirm-disposable-session"],
            about: "Offline preflight unless --submit. Requires exact --confirm-session-id and --confirm-disposable-session; never retries.",
        },
        "unknown-recovery" => Options {
            required: &["profile-home", "session-id"],
            values: &[],
            flags: &[],
            about: "Read-only recovery of a never-submitted identity in a temporary ledger; no submit or cancel.",
        },
        "reconcile-terminal" => Options {
            required: &["profile-home", "ledger", "submission-id"],
            values: &[],
            flags: &["execute"],
            about: "Offline unless --execute; only reads prompt.receipt and reconciles an existing regular ledger. Never submits or cancels.",
        },
        "validate-existing" => Options {
            required: &["profile-home"],
            values: &["session-id", "list-sessions", "limit"],
            flags: &[],
            about: "Read-only contract validation. --session-id and --list-sessions {live,stored} are mutually exclusive. --limit defaults to 10 (1-20).",
        },
        "ollama-smoke" => Options {
            required: &[],
            values: &[],
            flags: &[],
            about: "Runs bounded local worker/reviewer inference; no files applied.",
        },
        "functional-fixture" => Options {
            required: &[],
            values: &[],
            flags: &[],
            about: "Runs trusted passing/failing fixtures through Bubblewrap; never executes generated code on the host.",
        },
        _ => return None,
    })
}

/// Parse one subcommand's flags without accessing a gateway or filesystem.
pub fn parse(command: &str, argv: &[OsString]) -> Result<Args, ProbeError> {
    let spec = options(command).ok_or_else(|| ProbeError::value("Unknown probe subcommand"))?;
    let mut result = Args::default();
    let mut iter = argv.iter();
    while let Some(raw) = iter.next() {
        let raw = raw
            .to_str()
            .ok_or_else(|| ProbeError::value("Invalid option"))?;
        let (name, inline) = raw
            .split_once('=')
            .map_or((raw, None), |(k, v)| (k, Some(v)));
        let name = name
            .strip_prefix("--")
            .ok_or_else(|| ProbeError::value("Expected an option"))?;
        let candidates: Vec<_> = spec
            .required
            .iter()
            .chain(spec.values)
            .chain(spec.flags)
            .copied()
            // Only gateway probes (all have required options) take an endpoint.
            .chain((!spec.required.is_empty()).then_some("endpoint"))
            .filter(|candidate| *candidate == name || candidate.starts_with(name))
            .collect();
        let name = candidates
            .iter()
            .find(|candidate| **candidate == name)
            .copied()
            .or_else(|| {
                if candidates.len() == 1 {
                    candidates.first().copied()
                } else {
                    None
                }
            })
            .ok_or_else(|| ProbeError::value("Unknown or ambiguous option"))?;
        let value = if spec.flags.contains(&name) {
            if inline.is_some() {
                return Err(ProbeError::value("Flag does not take a value"));
            }
            OsString::new()
        } else if let Some(value) = inline {
            OsString::from(value)
        } else {
            let value = iter
                .next()
                .ok_or_else(|| ProbeError::value("Option requires a value"))?;
            // argparse treats any dash-led token except a negative number as an option.
            let lossy = value.to_string_lossy();
            if lossy.starts_with('-') && lossy.parse::<f64>().is_err() {
                return Err(ProbeError::value("Option requires a value"));
            }
            value.clone()
        };
        // Identities are compared as text; only path options may carry non-UTF-8 bytes.
        if !PATH_OPTIONS.contains(&name) && value.to_str().is_none() {
            return Err(ProbeError::value("Option value must be UTF-8"));
        }
        result.values.insert(name.into(), value);
    }
    for name in spec.required {
        if !result.has(name) {
            return Err(ProbeError::value(&format!("--{name} is required")));
        }
    }
    if command == "validate-existing" {
        if result.has("session-id") && result.has("list-sessions") {
            return Err(ProbeError::value(
                "--session-id and --list-sessions are mutually exclusive",
            ));
        }
        if result.has("list-sessions")
            && !["live", "stored"].contains(&result.text("list-sessions"))
        {
            return Err(ProbeError::value("Unsupported session scope"));
        }
        if result.has("limit") && result.text("limit").parse::<i64>().is_err() {
            return Err(ProbeError::value("--limit must be an integer"));
        }
    }
    Ok(result)
}

/// Execute a parsed subcommand with an attach-only injected gateway connector.
pub fn execute<C: Connector>(
    command: &str,
    args: &Args,
    connector: &mut C,
) -> Result<Value, ProbeError> {
    match command {
        "disposable-admission" => disposable_admission::run(args, connector),
        "live-admission" => live_admission::run(args, connector),
        "unknown-recovery" => unknown_recovery::run(args, connector),
        "reconcile-terminal" => reconcile_terminal::run(args, connector),
        "validate-existing" => validate_existing::run(args, connector),
        "ollama-smoke" => ollama_smoke::run(),
        "functional-fixture" => functional_fixture::run(),
        _ => Err(ProbeError::value("Unknown probe subcommand")),
    }
}

/// CLI boundary with injectable gateway effects and sanitized runtime failures.
pub fn run_with<C: Connector>(
    argv: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    connector: &mut C,
) -> i32 {
    let command = argv.first().and_then(|arg| arg.to_str()).unwrap_or("");
    let rest = argv.get(1..).unwrap_or_default();
    if ["--help", "-h"].contains(&command) {
        let _ = writeln!(
            stdout,
            "hermes-probe <subcommand>\ndisposable-admission live-admission unknown-recovery reconcile-terminal validate-existing ollama-smoke functional-fixture"
        );
        return 0;
    }
    let Some(spec) = options(command) else {
        let _ = writeln!(stderr, "hermes-probe: error: Unknown probe subcommand");
        return 2;
    };
    if rest.iter().any(|arg| arg == "--help" || arg == "-h") {
        let _ = writeln!(
            stdout,
            "hermes-probe {command}\n{}\n--endpoint <unix socket path>: explicit existing endpoint; no discovery or autostart",
            spec.about
        );
        for name in spec.required.iter().chain(spec.values).chain(spec.flags) {
            let _ = writeln!(
                stdout,
                "--{name}{}",
                if spec.required.contains(name) {
                    " (required)"
                } else {
                    ""
                }
            );
        }
        return 0;
    }
    let parsed = match parse(command, rest) {
        Ok(parsed) => parsed,
        Err(error) => {
            let _ = writeln!(stderr, "hermes-probe: error: {error}");
            return 2;
        }
    };
    match execute(command, &parsed, connector) {
        Ok(value) => {
            let _ = writeln!(stdout, "{value}");
            0
        }
        Err(error) => {
            // Local probes touch no gateway, so their reason is safe and needed on stderr.
            if error.message == CLIENT_UNAVAILABLE
                || matches!(command, "ollama-smoke" | "functional-fixture")
            {
                let _ = writeln!(stderr, "hermes-probe: {}", error.message);
            }
            let _ = writeln!(
                stdout,
                "{}",
                json!({"result":"unavailable", "error_type":error.category})
            );
            2
        }
    }
}

/// Production stdio entry point.
pub fn run(argv: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    run_with(argv, stdout, stderr, &mut ProductionConnector)
}
