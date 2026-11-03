//! Read-only host control inspection; no service launch or automatic approval.

use std::ffi::OsStr;
use std::io::Write;

use clap::Parser;

use crate::util::bin_main::parse_args;

use super::live_host_probe::{
    CommandOutput, HostProbeError, ProbeCommands, Runner, assess_observation, observe_unit,
    operator_runner,
};

const USAGE: &str = "usage: live_host_probe_cli [-h] --unit UNIT [--execute-readonly]";

#[derive(Parser)]
#[command(args_override_self = true, infer_long_args = true)]
struct Args {
    #[arg(long)]
    unit: String,
    #[arg(long)]
    execute_readonly: bool,
}

/// Inspect a named disposable unit with an injected runner; no live Hermes or provider execution.
pub fn run_with(
    args: &[impl AsRef<OsStr>],
    runner: Runner<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let parsed: Args = match parse_args(
        "live_host_probe_cli",
        args,
        "Inspect fixed systemd sandbox properties",
        USAGE,
        stdout,
        stderr,
    ) {
        Ok(parsed) => parsed,
        Err(status) => return status,
    };
    let commands = match ProbeCommands::new(&parsed.unit) {
        Ok(commands) => commands,
        Err(exc) => return failed(stderr, &exc),
    };
    if !parsed.execute_readonly {
        let _ = writeln!(
            stderr,
            "{USAGE}\nlive_host_probe_cli: error: Pass --execute-readonly to query systemd; \
             default is disabled"
        );
        return 2;
    }
    let values = match observe_unit(&commands, runner) {
        Ok(values) => values,
        Err(exc) => return failed(stderr, &exc),
    };
    let failures = assess_observation(&values);
    for (name, value) in &values {
        let _ = writeln!(stdout, "{name}={value}");
    }
    let summary = if failures.is_empty() {
        "none".to_owned()
    } else {
        failures.join(", ")
    };
    let _ = writeln!(stdout, "Control mismatches: {summary}");
    let _ = writeln!(stdout, "Gateway authorization: NOT GRANTED");
    i32::from(!failures.is_empty())
}

fn failed(stderr: &mut dyn Write, exc: &HostProbeError) -> i32 {
    let _ = writeln!(stderr, "HostProbeError: {exc}");
    1
}

/// Command-line entry point; `args` excludes the program name. Returns the exit code.
pub fn run(args: &[impl AsRef<OsStr>], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let runner =
        |argv: &[String]| -> Result<CommandOutput, HostProbeError> { operator_runner(argv) };
    run_with(args, &runner, stdout, stderr)
}
