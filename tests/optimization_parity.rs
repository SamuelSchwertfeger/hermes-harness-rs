#![allow(missing_docs, clippy::unwrap_used)]
//! Regression coverage for host observations and Python whitespace parity.

use std::time::Duration;

use hermes_harness::deployment::gpu::{ProbeOutput, ProbeRunner, observed_free_vram_mib_with};
use hermes_harness::multiagent::runtime::WorkflowError;

struct Observation<'a> {
    argv: &'a [&'a str],
    stdout: &'a str,
}

impl ProbeRunner for Observation<'_> {
    fn run(&self, argv: &[&str], timeout: Duration) -> Result<ProbeOutput, WorkflowError> {
        assert_eq!(argv, self.argv);
        assert_eq!(timeout, Duration::from_secs(8));
        Ok(ProbeOutput {
            returncode: 0,
            stdout: self.stdout.to_owned(),
        })
    }
}

#[test]
fn hostname_seam_uses_mock_observation() {
    let host = Observation {
        argv: &["hostname"],
        stdout: "mock-nodename\n",
    };
    assert_eq!(host.hostname().unwrap(), "mock-nodename");
}

#[cfg(unix)]
#[test]
fn hostname_seams_return_nodename() {
    use hermes_harness::deployment::gpu::CommandRunner;
    use hermes_harness::native::repair_dispatch::{DispatchHost as _, SystemHost};

    let uname = rustix::system::uname();
    let expected = uname.nodename().to_string_lossy();
    assert_eq!(CommandRunner.hostname().unwrap(), expected);
    assert_eq!(SystemHost.hostname(), expected);
}

#[test]
fn vram_observation_strips_python_separators() {
    let host = Observation {
        argv: &[
            "/usr/bin/nvidia-smi",
            "--query-gpu=memory.free",
            "--format=csv,noheader,nounits",
        ],
        stdout: "\u{1c}4096\u{1f}\n",
    };
    assert_eq!(observed_free_vram_mib_with(&host).unwrap(), 4096);
}
