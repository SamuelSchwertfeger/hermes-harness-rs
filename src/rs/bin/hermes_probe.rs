//! Consolidated operator probe entry point.

fn main() -> std::process::ExitCode {
    hermes_harness::util::bin_main::run_stdio(hermes_harness::probe::run)
}
