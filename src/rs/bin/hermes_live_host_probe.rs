//! `hermes-live-host-probe` entry point.

use std::process::ExitCode;

use hermes_harness::operator::live_host_probe_cli;
use hermes_harness::util::bin_main::run_stdio;

fn main() -> ExitCode {
    run_stdio(live_host_probe_cli::run)
}
