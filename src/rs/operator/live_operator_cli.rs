//! Operator-only read-only cross-ledger inspection; never starts Hermes.

use std::ffi::OsStr;
use std::io::Write;

use clap::Parser;

use crate::util::bin_main::parse_args;
use std::path::PathBuf;

use super::live_operator_report::{LedgerPaths, inspect_task};

const USAGE: &str = "usage: live_operator_cli [-h] --bindings BINDINGS --services SERVICES \
                     --admissions ADMISSIONS --task-id TASK_ID";

#[derive(Parser)]
#[command(args_override_self = true, infer_long_args = true)]
struct Args {
    #[arg(long)]
    bindings: PathBuf,
    #[arg(long)]
    services: PathBuf,
    #[arg(long)]
    admissions: PathBuf,
    #[arg(long)]
    task_id: String,
}

/// Command-line entry point; `args` excludes the program name. Returns the exit code.
pub fn run(args: &[impl AsRef<OsStr>], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let parsed: Args = match parse_args(
        "live_operator_cli",
        args,
        "Read-only supervised live task inspection",
        USAGE,
        stdout,
        stderr,
    ) {
        Ok(parsed) => parsed,
        Err(status) => return status,
    };
    let paths = LedgerPaths {
        bindings: parsed.bindings,
        services: parsed.services,
        admissions: parsed.admissions,
    };
    let rendered = inspect_task(&paths, &parsed.task_id)
        .map_err(|exc| exc.to_string())
        .and_then(|report| report.to_json().map_err(|exc| exc.to_string()));
    match rendered {
        Ok(text) => {
            let _ = writeln!(stdout, "{text}");
            0
        }
        Err(message) => {
            let _ = writeln!(stderr, "LiveReportError: {message}");
            1
        }
    }
}
