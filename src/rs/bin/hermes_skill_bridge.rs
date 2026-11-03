//! `hermes-skill-bridge` entry point.

use std::process::ExitCode;

use hermes_harness::native::skill_bridge::{Args, render_error, run};
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
                match render_error(&error) {
                    Ok(line) => println!("{line}"),
                    Err(encode) => eprintln!("hermes-skill-bridge: {error}: {encode}"),
                }
                2
            }
        }
    })
}
