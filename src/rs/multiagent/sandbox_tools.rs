//! Explicitly authorized, read-only Bubblewrap tool calls for local agents.
//!
//! This is a narrow primitive, not a general shell and not a live orchestration loop.
//! No executable, shell string, or filesystem path is selected by the model.

use crate::multiagent::runtime::fail;
use crate::util::PathLexical as _;
use std::path::{Path, PathBuf};

use serde_json::json;
#[cfg(unix)]
use sha2::{Digest as _, Sha256};

use crate::multiagent::runtime::WorkflowError;
use crate::util::{resolve_strict, sha256_hex, sorted_json_spaced_value};

/// Maximum bytes of stdout and of stderr kept from one tool call.
pub const MAX_OUTPUT: usize = 16384;
/// Wall-clock and CPU budget of one tool call in seconds.
pub const MAX_SECONDS: u64 = 30;
/// Maximum length (in characters) of a fixed-string search pattern.
pub const MAX_PATTERN: usize = 256;

/// Default sandbox label recorded in every receipt.
const SANDBOX_LABEL: &str = "bubblewrap-no-network-readonly-worktree";

crate::util::wire_enum! {
    /// One of the three preapproved read-only tools.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
    pub enum SandboxTool {
        /// List worktree files.
        ListFiles => "list-files",
        /// Search for a bounded fixed string.
        SearchFixed => "search-fixed",
        /// Read Git status.
        GitStatus => "git-status",
    }

    /// Python tool name used on the wire.
    as_str;
    from_str(value) -> WorkflowError { WorkflowError::msg("Unknown sandbox tool") }
}

/// Captured outcome of one sandboxed tool call.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolResult {
    /// Tool name (`list-files`, `search-fixed` or `git-status`).
    pub tool: SandboxTool,
    /// Process exit code; negative for a terminating signal.
    pub exit_code: Option<i32>,
    /// Whether the call timed out.
    pub timed_out: bool,
    /// Captured standard output, truncated to [`MAX_OUTPUT`] bytes.
    pub stdout: String,
    /// Captured standard error, truncated to [`MAX_OUTPUT`] bytes.
    pub stderr: String,
    /// SHA-256 over `stdout + NUL + stderr` bytes.
    pub output_sha256: String,
    /// Sandbox profile label.
    pub sandbox: String,
    /// Always false: nothing runs on the host.
    pub host_execution: bool,
}

impl ToolResult {
    /// Build a result with the default sandbox label and `host_execution = false`.
    #[must_use]
    pub fn new(
        tool: SandboxTool,
        exit_code: Option<i32>,
        timed_out: bool,
        stdout: &str,
        stderr: &str,
        output_sha256: &str,
    ) -> Self {
        Self {
            tool,
            exit_code,
            timed_out,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            output_sha256: output_sha256.to_owned(),
            sandbox: SANDBOX_LABEL.to_owned(),
            host_execution: false,
        }
    }
}

fn binary_present(binary: &str) -> bool {
    Path::new(binary).is_file()
}

/// Generate only preapproved static commands; reject user-controlled argv.
///
/// # Errors
/// Returns [`WorkflowError`] for an unsafe worktree, broad root, unknown tool, invalid
/// pattern or missing `/usr/bin/rg` / `/usr/bin/git`.
pub fn sandbox_tool_argv(
    worktree: &Path,
    tool: &str,
    pattern: &str,
    bwrap: &str,
) -> Result<Vec<String>, WorkflowError> {
    sandbox_tool_argv_with(worktree, tool, pattern, bwrap, &binary_present)
}

/// [`sandbox_tool_argv`] with an injectable presence check for `/usr/bin/rg` and
/// `/usr/bin/git`.
///
/// # Errors
/// Same as [`sandbox_tool_argv`].
pub fn sandbox_tool_argv_with(
    worktree: &Path,
    tool: &str,
    pattern: &str,
    bwrap: &str,
    present: &dyn Fn(&str) -> bool,
) -> Result<Vec<String>, WorkflowError> {
    if !worktree.is_absolute() || worktree.is_symlink_lexical() || !worktree.is_dir() {
        return fail("Worktree must be an existing absolute real directory");
    }
    let root = resolve_strict(worktree)?;
    if [Path::new("/"), Path::new("/home"), Path::new("/tmp")].contains(&root.as_path()) {
        return fail("Refusing broad sandbox root");
    }
    let command = tool_command(tool.parse()?, pattern)?;
    if !["/usr/bin/rg", "/usr/bin/git"].iter().all(|b| present(b)) {
        return fail("Required sandbox tool unavailable");
    }
    let mut argv: Vec<String> = [
        bwrap,
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--clearenv",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--setenv",
        "HOME",
        "/tmp",
        "--setenv",
        "GIT_CONFIG_NOSYSTEM",
        "1",
        "--setenv",
        "GIT_CONFIG_GLOBAL",
        "/dev/null",
        "--setenv",
        "GIT_OPTIONAL_LOCKS",
        "0",
        "--tmpfs",
        "/tmp",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]
    .iter()
    .map(|item| (*item).to_owned())
    .collect();
    for directory in ["/usr", "/bin", "/lib", "/lib64"] {
        if Path::new(directory).exists() {
            argv.extend([
                "--ro-bind".to_owned(),
                directory.to_owned(),
                directory.to_owned(),
            ]);
        }
    }
    argv.extend(["--ro-bind".to_owned(), root.to_string_lossy().into_owned()]);
    argv.extend(["/work", "--chdir", "/work", "--"].map(str::to_owned));
    argv.extend(command);
    Ok(argv)
}

fn tool_command(tool: SandboxTool, pattern: &str) -> Result<Vec<String>, WorkflowError> {
    let command: Vec<&str> = match tool {
        SandboxTool::ListFiles => vec!["/usr/bin/rg", "--files", "--hidden", "-g", "!.git"],
        SandboxTool::SearchFixed => {
            if pattern.is_empty()
                || pattern.chars().count() > MAX_PATTERN
                || pattern.contains('\0')
                || pattern.contains('\n')
            {
                return fail("Invalid bounded search pattern");
            }
            vec![
                "/usr/bin/rg",
                "-n",
                "--fixed-strings",
                "-e",
                pattern,
                "--",
                ".",
            ]
        }
        SandboxTool::GitStatus => vec!["/usr/bin/git", "status", "--short", "--untracked-files=no"],
    };
    Ok(command.into_iter().map(str::to_owned).collect())
}

/// Look up an executable on `PATH` like Python `shutil.which`.
#[must_use]
pub(crate) fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|directory| {
        which_candidates(&directory, name)
            .into_iter()
            .find(|candidate| is_executable_file(candidate))
    })
}

#[cfg(windows)]
fn which_candidates(directory: &Path, name: &str) -> Vec<PathBuf> {
    let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    let mut found = vec![directory.join(name)];
    found.extend(
        extensions
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(|ext| directory.join(format!("{name}{ext}"))),
    );
    found
}

#[cfg(not(windows))]
fn which_candidates(directory: &Path, name: &str) -> Vec<PathBuf> {
    vec![directory.join(name)]
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Raw capture of one limited child process.
#[cfg(unix)]
pub(crate) struct Captured {
    pub(crate) code: Option<i32>,
    pub(crate) timed_out: bool,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Anonymous private scratch file (Python `tempfile.TemporaryFile`).
#[cfg(unix)]
fn anonymous_file(prefix: &str) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = std::env::temp_dir().join(format!("{prefix}{}", crate::util::random_hex(8)?));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    Ok(file)
}

#[cfg(unix)]
fn read_head(file: &mut std::fs::File, max_output: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut buffer = Vec::new();
    file.take(max_output as u64).read_to_end(&mut buffer)?;
    Ok(buffer)
}

#[cfg(unix)]
fn exit_code_of(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
}

/// Run `argv` under `prlimit` (CPU, core, file size, address space), no host fallback.
///
/// `unsafe` is forbidden, so Python's `preexec_fn=setrlimit` is replaced by util-linux
/// `prlimit` applying the identical limits before it execs the command.
#[cfg(unix)]
fn run_limited(argv: &[String]) -> Result<Captured, WorkflowError> {
    run_limited_with(
        argv,
        "hh-sbx-",
        MAX_OUTPUT,
        MAX_SECONDS,
        &[
            &format!("--cpu={MAX_SECONDS}"),
            "--core=0",
            &format!("--fsize={MAX_OUTPUT}"),
            &format!("--as={}", 1024_u64 * 1024 * 1024),
        ],
    )
}

/// Shared limited runner; callers retain their limit argument order and scratch prefix.
#[cfg(unix)]
pub(crate) fn run_limited_with(
    argv: &[String],
    scratch_prefix: &str,
    max_output: usize,
    max_seconds: u64,
    limits: &[&str],
) -> Result<Captured, WorkflowError> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let Some(limiter) = ["/usr/bin/prlimit", "/bin/prlimit"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
    else {
        return fail("Resource limiter unavailable: refusing unconstrained execution");
    };
    let mut stdout_file = anonymous_file(scratch_prefix)?;
    let mut stderr_file = anonymous_file(scratch_prefix)?;
    let mut child = Command::new(limiter)
        .args(limits)
        .arg("--")
        .args(argv)
        .current_dir("/")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::from(stderr_file.try_clone()?))
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(max_seconds);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    Ok(Captured {
        code: status.and_then(exit_code_of),
        timed_out: status.is_none(),
        stdout: read_head(&mut stdout_file, max_output)?,
        stderr: read_head(&mut stderr_file, max_output)?,
    })
}

/// No host fallback if Bubblewrap is absent or refuses namespace creation.
///
/// # Errors
/// Returns [`WorkflowError`] without operator authorization, when Bubblewrap is missing,
/// when the argv is refused, or when the sandbox fails or times out.
pub fn run_sandbox_tool(
    worktree: &Path,
    tool: &str,
    pattern: &str,
    authorized: bool,
) -> Result<ToolResult, WorkflowError> {
    run_sandbox_tool_with(worktree, tool, pattern, authorized, &which)
}

/// Run a sandbox tool with caller-supplied executable discovery.
///
/// # Security
/// Production callers must use [`run_sandbox_tool`]. An injected `which` implementation
/// that returns a non-Bubblewrap binary runs the tool unsandboxed.
///
/// # Errors
/// Same as [`run_sandbox_tool`].
pub fn run_sandbox_tool_with(
    worktree: &Path,
    tool: &str,
    pattern: &str,
    authorized: bool,
    find: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<ToolResult, WorkflowError> {
    if !authorized {
        return fail("Separate operator authorization required for sandbox tool execution");
    }
    let Some(bwrap) = find("bwrap") else {
        return fail("Bubblewrap unavailable: refusing host execution");
    };
    let argv = sandbox_tool_argv(worktree, tool, pattern, &bwrap.to_string_lossy())?;
    execute(tool.parse()?, &argv)
}

#[cfg(unix)]
fn execute(tool: SandboxTool, argv: &[String]) -> Result<ToolResult, WorkflowError> {
    let captured = run_limited(argv)?;
    if sandbox_failed(captured.timed_out, &captured.stderr) {
        return fail("Sandbox unavailable or timed out; no host fallback");
    }
    let mut hashed = Sha256::new();
    hashed.update(&captured.stdout);
    hashed.update([0]);
    hashed.update(&captured.stderr);
    Ok(ToolResult {
        tool,
        exit_code: captured.code,
        timed_out: false,
        stdout: String::from_utf8_lossy(&captured.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&captured.stderr).into_owned(),
        output_sha256: hex::encode(hashed.finalize()),
        sandbox: SANDBOX_LABEL.to_owned(),
        host_execution: false,
    })
}

#[cfg(any(unix, test))]
fn sandbox_failed(timed_out: bool, stderr: &[u8]) -> bool {
    timed_out
        || stderr.starts_with(b"bwrap:")
        || stderr.starts_with(b"bubblewrap:")
        || stderr.starts_with(b"prlimit:")
}

#[cfg(not(unix))]
fn execute(_tool: SandboxTool, _argv: &[String]) -> Result<ToolResult, WorkflowError> {
    fail("Bubblewrap unavailable: refusing host execution")
}

/// Store an immutable receipt; never overwrite previous or uncertain work.
///
/// # Errors
/// Returns [`WorkflowError`] when the destination exists, is a symlink or has no existing
/// parent directory, or when writing fails.
pub fn record_sandbox_tool(result: &ToolResult, evidence: &Path) -> Result<String, WorkflowError> {
    use std::io::Write as _;

    let parent = match evidence.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => Path::new(""),
    };
    if evidence.is_symlink_lexical() || evidence.exists() || !parent.is_dir() {
        return fail("Evidence destination must be a new path in an existing directory");
    }
    let receipt = json!({
        "tool": result.tool,
        "exit_code": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "output_sha256": result.output_sha256,
        "sandbox": result.sandbox,
        "host_execution": result.host_execution,
        "timed_out": result.timed_out,
    });
    let payload = format!("{}\n", sorted_json_spaced_value(&receipt)?).into_bytes();
    let mut target = open_new_private(evidence)?;
    target.write_all(&payload)?;
    target.flush()?;
    target.sync_all()?;
    Ok(sha256_hex(&payload))
}

fn open_new_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let nofollow = i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or(0);
        options.mode(0o600).custom_flags(nofollow);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::sandbox_failed;

    #[test]
    fn sandbox_failure_refusal() {
        for (timed_out, stderr, expected) in [
            (true, b"".as_slice(), true),
            (false, b"bwrap: setting up uid map: Permission denied", true),
            (false, b"bubblewrap: namespace creation failed", true),
            (false, b"prlimit: failed to set limit", true),
            (false, b"rg: normal search diagnostic", false),
            (false, b"", false),
        ] {
            assert_eq!(sandbox_failed(timed_out, stderr), expected);
        }
    }
}
