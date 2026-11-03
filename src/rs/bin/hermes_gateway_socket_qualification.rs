//! `hermes-gateway-socket-qualification` entry point.

use std::process::ExitCode;

use hermes_harness::gateway::gateway_socket_qualification::main as qualify;
use hermes_harness::util::bin_main::{logged, status_byte};

fn main() -> ExitCode {
    logged(|| {
        let argv: Vec<_> = std::env::args_os().collect();
        status_byte(qualify(&argv))
    })
}
