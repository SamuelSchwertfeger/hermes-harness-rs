//! `hermes-native` entry point.

use std::process::ExitCode;

use hermes_harness::native::main_cli::{Args, run};
use hermes_harness::util::bin_main::{logged, parse_or_status};

#[allow(clippy::print_stderr, reason = "the binary reports failures on stderr")]
fn main() -> ExitCode {
    logged(|| {
        let args = match parse_or_status::<Args>() {
            Ok(args) => args,
            Err(code) => return code,
        };
        match run(&args) {
            Ok(output) => {
                println!("{output}");
                0
            }
            Err(error) => {
                match error.report() {
                    Ok(line) => eprintln!("{line}"),
                    Err(encode) => eprintln!("hermes-native: {error}: {encode}"),
                }
                2
            }
        }
    })
}
