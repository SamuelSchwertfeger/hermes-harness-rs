//! Process entry glue shared by the thin `src/rs/bin/*.rs` binaries.

use std::ffi::{OsStr, OsString};
use std::fmt::Display;

use clap::Parser;
use std::io::{self, Write};
use std::process::ExitCode;

/// Signature of the library entry points that take argv and the two output streams.
pub type StreamEntry = fn(&[OsString], &mut dyn Write, &mut dyn Write) -> i32;

/// Process arguments after the program name.
#[must_use]
pub fn user_args() -> Vec<OsString> {
    std::env::args_os().skip(1).collect()
}

/// Map a Python-style integer exit status onto a process exit byte.
#[must_use]
pub fn status_byte(status: i32) -> u8 {
    u8::try_from(status & 0xff).unwrap_or(1)
}

/// Map a Python-style integer exit status onto a process exit code.
#[must_use]
pub fn exit_code(status: i32) -> ExitCode {
    ExitCode::from(status_byte(status))
}

/// Name of the running binary (file stem of argv[0]); never includes arguments.
fn bin_name() -> String {
    std::env::args_os()
        .next()
        .and_then(|arg| {
            std::path::Path::new(&arg)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "hermes".to_owned())
}

/// Install the opt-in JSON logger, run `body`, and log start and exit (never the arguments).
#[must_use]
pub fn logged(body: impl FnOnce() -> u8) -> ExitCode {
    let bin = bin_name();
    super::logging::init(&bin);
    let started = std::time::Instant::now();
    log::info!("start bin={bin}");
    let code = body();
    log::info!(
        "exit bin={bin} code={code} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    ExitCode::from(code)
}

/// Print a clap parse failure (help and version to stdout, errors to stderr, as `exit()` would)
/// and return its exit status, so the caller can return normally and the exit line gets logged.
#[must_use]
pub fn usage_status(error: &clap::Error) -> u8 {
    let _ = error.print();
    status_byte(error.exit_code())
}

/// Parse the process arguments, or print the clap response and return the exit status.
///
/// # Errors
/// Returns the exit status after printing help, version or the usage error.
pub fn parse_or_status<T: Parser>() -> Result<T, u8> {
    T::try_parse().map_err(|error| usage_status(&error))
}

/// Run a library entry point against the real standard streams.
#[must_use]
pub fn run_stdio(entry: StreamEntry) -> ExitCode {
    logged(|| {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        status_byte(run_streams(entry, &user_args(), &mut stdout, &mut stderr))
    })
}

/// Print the text of a successful run verbatim, or report the failure on stderr.
///
/// `failure_status` maps the error to the exit status Python would produce.
#[must_use]
pub fn emit_status<E: Display>(
    name: &str,
    result: Result<String, E>,
    failure_status: impl FnOnce(&E) -> u8,
) -> u8 {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    match result {
        Ok(text) => status_byte(write_output(&mut stdout, &mut stderr, &text)),
        Err(error) => {
            let _ = stderr.write_all(
                format!(
                    "{name}: error: {error}
"
                )
                .as_bytes(),
            );
            failure_status(&error)
        }
    }
}

fn write_output(stdout: &mut dyn Write, stderr: &mut dyn Write, text: &str) -> i32 {
    output_status(
        0,
        stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush()),
        stderr,
    )
}

fn output_status(status: i32, result: io::Result<()>, stderr: &mut dyn Write) -> i32 {
    if let Err(error) = result {
        let _ = stderr.write_all(
            format!(
                "stdout: {error}
"
            )
            .as_bytes(),
        );
        1
    } else {
        status
    }
}

struct CheckedWriter<'a> {
    inner: &'a mut dyn Write,
    failed: bool,
}

impl Write for CheckedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(bytes);
        self.failed |= result.is_err() || matches!(result, Ok(0)) && !bytes.is_empty();
        result
    }

    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        self.failed |= result.is_err();
        result
    }
}

fn run_streams(
    entry: StreamEntry,
    args: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let mut checked = CheckedWriter {
        inner: stdout,
        failed: false,
    };
    let status = entry(args, &mut checked, stderr);
    let flushed = checked.flush();
    if checked.failed {
        output_status(
            status,
            Err(io::Error::other("output write or flush failed")),
            stderr,
        )
    } else {
        output_status(status, flushed, stderr)
    }
}

/// Parse OS-native arguments while retaining the established CLI help and error prefix.
///
/// # Errors
/// Returns the help or usage exit status after writing the parser response.
pub(crate) fn parse_args<T: Parser>(
    name: &str,
    args: &[impl AsRef<OsStr>],
    help: &str,
    usage: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<T, i32> {
    T::try_parse_from(
        std::iter::once(OsString::from(name))
            .chain(args.iter().map(|arg| arg.as_ref().to_os_string())),
    )
    .map_err(|error| {
        if error.use_stderr() {
            let _ = writeln!(stderr, "{usage}\n{name}: error: {error}");
        } else {
            let _ = writeln!(stdout, "{usage}\n\n{help}");
        }
        error.exit_code()
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    struct FailingOutput {
        fail_write: bool,
        zero_write: bool,
    }

    impl Write for FailingOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_write {
                Err(io::Error::other("write failed"))
            } else if self.zero_write {
                Ok(0)
            } else {
                Ok(bytes.len())
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    #[test]
    fn stdout_failures_override_success() {
        for (fail_write, zero_write) in [(true, false), (false, false), (false, true)] {
            let mut output = FailingOutput {
                fail_write,
                zero_write,
            };
            let mut errors = Vec::new();
            assert_eq!(write_output(&mut output, &mut errors, "result"), 1);
            assert!(errors.starts_with(b"stdout:"));
            errors.clear();
            let entry: StreamEntry = |_, stdout, _| {
                let _ = stdout.write_all(b"result");
                0
            };
            assert_eq!(run_streams(entry, &[], &mut output, &mut errors), 1);
            assert!(errors.starts_with(b"stdout:"));
        }
    }

    #[test]
    fn stdout_success_keeps_status_and_bytes() {
        let mut output = Vec::new();
        let mut errors = Vec::new();
        assert_eq!(write_output(&mut output, &mut errors, "result\n"), 0);
        assert_eq!(output, b"result\n");
        let entry: StreamEntry = |_, stdout, _| {
            let _ = stdout.write_all(b"more\n");
            2
        };
        assert_eq!(run_streams(entry, &[], &mut output, &mut errors), 2);
        assert_eq!(output, b"result\nmore\n");
        assert_eq!(errors, b"");
    }
}
