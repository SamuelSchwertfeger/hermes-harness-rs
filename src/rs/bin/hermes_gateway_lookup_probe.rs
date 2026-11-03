//! `hermes-gateway-lookup-probe` entry point.

use std::process::ExitCode;

use hermes_harness::gateway::gateway_lookup_probe;
use hermes_harness::util::bin_main::run_stdio;

fn main() -> ExitCode {
    run_stdio(gateway_lookup_probe::run)
}
