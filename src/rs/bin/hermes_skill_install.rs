//! `hermes-skill-install` entry point.

use std::process::ExitCode;

use hermes_harness::native::skill_install::{SkillInstallArgs, run_with};
use hermes_harness::util::bin_main::{emit_status, logged, parse_or_status};

fn main() -> ExitCode {
    logged(|| {
        let args = match parse_or_status::<SkillInstallArgs>() {
            Ok(args) => args,
            Err(code) => return code,
        };
        emit_status(
            "hermes-skill-install",
            run_with(&args, &mut std::io::stdout().lock()).map(|()| String::new()),
            |_| 1,
        )
    })
}
