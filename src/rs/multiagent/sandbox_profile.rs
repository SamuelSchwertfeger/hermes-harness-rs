//! Linux bubblewrap command construction for independent verifier qualification.
//!
//! This is a proposed OS isolation profile, not an attested sandbox. It is
//! deliberately *not* wired to automatic execution or trusted-code flags.
//! Operator must qualify user namespaces, filesystem mounts, /proc, seccomp,
//! resource limits and device exposure on the actual Debian host first.

use crate::multiagent::runtime::fail;
use crate::util::PathLexical as _;
use std::path::{Path, PathBuf};

use crate::multiagent::runtime::WorkflowError;
use crate::util::resolve_lenient;

/// Upper bound for the sandbox wall-clock budget in seconds.
pub const MAX_SANDBOX_SECONDS: i64 = 180;
/// Maximum number of arguments after the executable.
pub const MAX_SANDBOX_ARGS: usize = 16;
/// Maximum length (in characters) of one argument.
pub const MAX_SANDBOX_ARG_LENGTH: usize = 512;
/// Private bubblewrap tmpfs, not the host temporary directory.
pub const SANDBOX_TMP: &str = "/tmp";

/// Worktree, executable, arguments and time budget of one sandboxed command.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SandboxSpec {
    /// Absolute, non-symlink worktree directory.
    pub worktree: PathBuf,
    /// Absolute, non-symlink executable file.
    pub executable: PathBuf,
    /// Command arguments (1 to [`MAX_SANDBOX_ARGS`]).
    pub args: Vec<String>,
    /// Time budget in seconds (1 to [`MAX_SANDBOX_SECONDS`]).
    pub max_seconds: i64,
}

impl SandboxSpec {
    /// Build a spec with the default 60 second budget.
    #[must_use]
    pub fn new(worktree: &Path, executable: &Path, args: &[&str]) -> Self {
        Self {
            worktree: worktree.to_path_buf(),
            executable: executable.to_path_buf(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            max_seconds: 60,
        }
    }

    /// Replace the time budget.
    #[must_use]
    pub const fn with_max_seconds(mut self, max_seconds: i64) -> Self {
        self.max_seconds = max_seconds;
        self
    }
}

/// Produce fixed network-disabled, read-only-root argv without executing it.
///
/// # Errors
/// Returns [`WorkflowError`] when the worktree, executable, timeout or arguments are invalid.
pub fn bubblewrap_argv(spec: &SandboxSpec) -> Result<Vec<String>, WorkflowError> {
    let root = spec.worktree.as_path();
    let executable = spec.executable.as_path();
    let root_ok = root.is_absolute() && !root.is_symlink_lexical() && root.is_dir();
    let executable_ok =
        executable.is_absolute() && !executable.is_symlink_lexical() && executable.is_file();
    let duration_ok = (1..=MAX_SANDBOX_SECONDS).contains(&spec.max_seconds);
    if !(root_ok && executable_ok && duration_ok) {
        return fail("Invalid bubblewrap worktree, executable or timeout");
    }
    let resolved = resolve_lenient(root);
    if resolved == Path::new("/") || resolved == Path::new("/home") {
        return fail("Refusing overly broad sandbox worktree");
    }
    if spec.args.is_empty() || spec.args.len() > MAX_SANDBOX_ARGS {
        return fail("Invalid sandbox command cardinality");
    }
    if spec.args.iter().any(|item| {
        item.is_empty() || item.contains('\0') || item.chars().count() > MAX_SANDBOX_ARG_LENGTH
    }) {
        return fail("Invalid sandbox command argument");
    }
    // The code tree is mounted read-only and the process receives a private
    // /tmp. This is an isolation *proposal*, not proof of safe execution.
    let mut argv: Vec<String> = [
        "bwrap",
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--ro-bind",
        "/",
        "/",
        "--tmpfs",
        SANDBOX_TMP,
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--chdir",
    ]
    .iter()
    .map(|item| (*item).to_owned())
    .collect();
    argv.push(root.to_string_lossy().into_owned());
    for (name, value) in [
        ("HOME", SANDBOX_TMP),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("PYTEST_DISABLE_PLUGIN_AUTOLOAD", "1"),
        ("UV_OFFLINE", "1"),
    ] {
        argv.extend(["--setenv".to_owned(), name.to_owned(), value.to_owned()]);
    }
    argv.push("--".to_owned());
    argv.push(executable.to_string_lossy().into_owned());
    argv.extend(spec.args.iter().cloned());
    Ok(argv)
}
