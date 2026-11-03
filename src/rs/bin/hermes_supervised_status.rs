//! `hermes-supervised-status` entry point.

use std::process::ExitCode;

use hermes_harness::agents::supervised_status::{Args, render};
use hermes_harness::util::bin_main::{emit_status, logged, parse_or_status};

fn main() -> ExitCode {
    logged(|| {
        let args = match parse_or_status::<Args>() {
            Ok(args) => args,
            Err(code) => return code,
        };
        let result = render(&args).map(|json| format!("{json}\n"));
        emit_status("hermes-supervised-status", result, |_| 1)
    })
}
