//! Provider-free Unix socket boundary probe for a Bubblewrap net namespace.
//!
//! This deliberately implements no Hermes protocol, credentials, or provider traffic.

use std::io;
use std::path::Path;

use clap::Parser;
use serde_json::{Value, json};

use crate::util::pretty_json_value;

/// Exact probe token exchanged over the socket.
pub const TOKEN: &[u8] = b"hermes-local-probe-v1";

/// Failure of the probe (mirrors the Python exception classes it catches).
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// OS-level failure (Python `OSError`).
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The sandbox did not finish in time (Python `subprocess.TimeoutExpired`).
    #[error("sandbox probe timed out")]
    Timeout,
    /// Refused to run (Python `RuntimeError`).
    #[error("{0}")]
    Runtime(String),
}

impl ProbeError {
    /// The Python exception class name this error corresponds to.
    #[must_use]
    pub fn python_name(&self) -> &'static str {
        match self {
            Self::Timeout => "TimeoutExpired",
            Self::Runtime(_) => "RuntimeError",
            Self::Io(error) => io_error_name(error),
        }
    }
}

fn io_error_name(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => "TimeoutError",
        io::ErrorKind::ConnectionReset => "ConnectionResetError",
        io::ErrorKind::ConnectionAborted => "ConnectionAbortedError",
        io::ErrorKind::BrokenPipe => "BrokenPipeError",
        io::ErrorKind::NotFound => "FileNotFoundError",
        io::ErrorKind::PermissionDenied => "PermissionError",
        io::ErrorKind::AlreadyExists => "FileExistsError",
        _ => "OSError",
    }
}

/// Build a fixed, non-provider probe command with no ambient home mount.
#[must_use]
pub fn sandbox_command(directory: &Path) -> Vec<String> {
    let probe = concat!(
        "import socket,sys; s=socket.socket(socket.AF_UNIX); ",
        "s.settimeout(4); s.connect(\"/gateway/probe.sock\"); ",
        "s.sendall(b\"hermes-local-probe-v1\"); ",
        "assert s.recv(128)==b\"OK:hermes-local-probe-v1\"; ",
        "assert not __import__(\"os\").path.exists(\"/home/hermes/.hermes\"); ",
        "print(\"PASS: isolated Unix socket exchange\")",
    );
    let directory = directory.to_string_lossy();
    [
        "/usr/bin/bwrap",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--die-with-parent",
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind",
        "/bin",
        "/bin",
        "--ro-bind",
        "/lib",
        "/lib",
        "--ro-bind",
        "/lib64",
        "/lib64",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--ro-bind",
        &directory,
        "/gateway",
        "--chdir",
        "/",
        "/usr/bin/python3",
        "-c",
        probe,
    ]
    .iter()
    .map(|part| (*part).to_owned())
    .collect()
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Read as _, Write as _};
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::{ProbeError, TOKEN, io_error_name, sandbox_command};

    fn accept_with_timeout(listener: &UnixListener, limit: Duration) -> io::Result<UnixStream> {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + limit;
        loop {
            match listener.accept() {
                Ok((stream, _)) => return Ok(stream),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn serve_connection(listener: &UnixListener) -> io::Result<&'static str> {
        let mut connection = accept_with_timeout(listener, Duration::from_secs(8))?;
        connection.set_nonblocking(false)?;
        connection.set_read_timeout(Some(Duration::from_secs(5)))?;
        connection.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut buffer = [0_u8; 128];
        let received = connection.read(&mut buffer)?;
        if buffer.get(..received) != Some(TOKEN) {
            return Ok("invalid request");
        }
        let mut reply = b"OK:".to_vec();
        reply.extend_from_slice(TOKEN);
        connection.write_all(&reply)?;
        Ok("accepted exact probe")
    }

    /// Accept one connection and record what the sandboxed client sent.
    pub fn serve_once(listener: &UnixListener, outcome: &mut Vec<String>) {
        match serve_connection(listener) {
            Ok(text) => outcome.push(text.to_owned()),
            Err(error) => outcome.push(io_error_name(&error).to_owned()),
        }
    }

    fn effective_uid_is_root() -> bool {
        rustix::process::geteuid().is_root()
    }

    fn make_probe_dir() -> io::Result<PathBuf> {
        let base = std::env::temp_dir();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        for attempt in 0_u32..100 {
            let name = format!(
                "hermes-unix-probe-{:x}{:x}",
                std::process::id(),
                seed.wrapping_add(attempt)
            );
            let path = base.join(name);
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::ErrorKind::AlreadyExists.into())
    }

    fn run_sandbox(directory: &Path) -> Result<(i32, String), ProbeError> {
        let command = sandbox_command(directory);
        let (program, args) = command.split_first().ok_or(ProbeError::Timeout)?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut out_pipe = child.stdout.take();
        let mut err_pipe = child.stderr.take();
        let out_reader = thread::spawn(move || {
            let mut text = Vec::new();
            if let Some(stream) = out_pipe.as_mut() {
                let _ = stream.read_to_end(&mut text);
            }
            text
        });
        let err_reader = thread::spawn(move || {
            let mut text = Vec::new();
            if let Some(stream) = err_pipe.as_mut() {
                let _ = stream.read_to_end(&mut text);
            }
            text
        });
        let deadline = Instant::now() + Duration::from_secs(12);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ProbeError::Timeout);
            }
            thread::sleep(Duration::from_millis(20));
        };
        let stdout_bytes = out_reader.join().unwrap_or_default();
        let _ = err_reader.join();
        Ok((
            status.code().unwrap_or(-1),
            String::from_utf8_lossy(&stdout_bytes).into_owned(),
        ))
    }

    fn probe_in(directory: &Path) -> Result<Value, ProbeError> {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        let address = directory.join("probe.sock");
        let listener = UnixListener::bind(&address)?;
        std::fs::set_permissions(&address, std::fs::Permissions::from_mode(0o600))?;
        let mut outcome: Vec<String> = Vec::new();
        let sandbox = thread::scope(|scope| {
            let server = scope.spawn(|| serve_once(&listener, &mut outcome));
            let result = run_sandbox(directory);
            let _ = server.join();
            result
        });
        let (code, stdout) = sandbox?;
        let passed = code == 0 && outcome == ["accepted exact probe"];
        let trimmed: String = crate::util::py_strip(&stdout).chars().take(300).collect();
        Ok(json!({
            "sandbox_exit_code": code,
            "sandbox_stdout": trimmed,
            "broker_observation": outcome,
            "passed": passed,
            "provider_invoked": false,
            "gateway_protocol_qualified": false,
        }))
    }

    /// Run the sandboxed Unix socket exchange and report what both sides observed.
    ///
    /// # Errors
    /// Socket qualification is unavailable or socket connection, framing, or response validation
    /// fails.
    pub fn run_probe() -> Result<Value, ProbeError> {
        if effective_uid_is_root() {
            return Err(ProbeError::Runtime("Run as hermes, never root".to_owned()));
        }
        let directory = make_probe_dir()?;
        let result = probe_in(&directory);
        let _ = std::fs::remove_dir_all(&directory);
        result
    }
}

#[cfg(unix)]
pub use imp::{run_probe, serve_once};

/// Run the sandboxed Unix socket exchange (unsupported off unix).
///
/// # Errors
/// Socket qualification is unavailable or socket connection, framing, or response validation fails.
#[cfg(not(unix))]
pub fn run_probe() -> Result<Value, ProbeError> {
    Err(ProbeError::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "Unix domain sockets and Bubblewrap are unavailable on this platform",
    )))
}

#[derive(Parser)]
#[command(
    about = "Provider-free Unix socket boundary probe for a Bubblewrap net namespace.",
    args_override_self = true,
    infer_long_args = true
)]
struct Cli {
    #[arg(long, required = true)]
    qualify: bool,
}

/// Command-line entry point; returns the process exit code.
#[must_use]
pub fn main(args: &[impl AsRef<std::ffi::OsStr>]) -> i32 {
    if let Err(error) = Cli::try_parse_from(args.iter().map(|arg| arg.as_ref().to_os_string())) {
        let _ = error.print();
        return error.exit_code();
    }
    let report = run_probe().unwrap_or_else(
        |error| json!({"passed": false, "error": error.python_name(), "provider_invoked": false}),
    );
    match pretty_json_value(&report) {
        Ok(text) => println!("{text}"),
        Err(_) => return 1,
    }
    i32::from(report.get("passed") != Some(&Value::Bool(true)))
}
