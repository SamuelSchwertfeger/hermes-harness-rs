//! `hermes-supervised-demo` entry point.

use std::process::ExitCode;

use hermes_harness::agents::supervised_demo::{Args, DemoError, run};
use hermes_harness::util::bin_main::{emit_status, logged, parse_or_status};

fn main() -> ExitCode {
    logged(|| {
        let args = match parse_or_status::<Args>() {
            Ok(args) => args,
            Err(code) => return code,
        };
        let result = run(&args).map(|json| format!("{json}\n"));
        emit_status("hermes-supervised-demo", result, |error| {
            if matches!(error, DemoError::Usage(_)) {
                2
            } else {
                1
            }
        })
    })
}
