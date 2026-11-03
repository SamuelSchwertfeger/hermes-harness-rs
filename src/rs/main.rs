//! `hermes-harness` command-line entry point.

use std::process::ExitCode;

use hermes_harness::cli::{CliError, run_argv};
use hermes_harness::util::bin_main::{logged, usage_status};

#[allow(clippy::print_stderr, reason = "the binary reports failures on stderr")]
fn main() -> ExitCode {
    logged(|| match run_argv(std::env::args_os()) {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(CliError::Usage(error)) => usage_status(&error),
        Err(error) => {
            eprintln!("hermes-harness: {error}");
            1
        }
    })
}
