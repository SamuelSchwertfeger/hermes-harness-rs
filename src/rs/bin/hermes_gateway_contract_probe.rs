//! `hermes-gateway-contract-probe` entry point.

use std::process::ExitCode;

use hermes_harness::gateway::gateway_contract_probe;
use hermes_harness::util::bin_main::run_stdio;

fn main() -> ExitCode {
    run_stdio(gateway_contract_probe::run)
}
