//! Fail-closed Bubblewrap functional test gate for reviewed disposable worktrees.
//!
//! Generated code executes only inside an explicitly approved local sandbox. No arbitrary
//! shell commands, host fallback, commits, or repair writes.

use crate::util::PathLexical as _;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::lifecycle::{files, load};
use crate::git::worktrees::{audit_worktree, git};
use crate::multiagent::runtime::WorkflowError;
use crate::multiagent::sandbox_tools::which;
use crate::util::{pretty_json_value, py_path_str, resolve_lenient, resolve_strict, sha256_hex};

#[cfg(unix)]
const MAX_OUTPUT: usize = 16_384;
const MAX_RECEIPT: usize = 128 * 1024;
#[cfg(unix)]
const TIMEOUT_SECONDS: u64 = 90;
const PYTHON: &str = "/usr/bin/python3";

fn require(condition: bool, message: &str) -> Result<(), WorkflowError> {
    if condition {
        Ok(())
    } else {
        Err(WorkflowError::msg(message))
    }
}

/// Build a fixed, network-disabled, no-host-home sandbox command.
#[must_use]
pub fn sandbox_argv(bwrap: &str, workspace: &Path, python: &str) -> Vec<String> {
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
        "PYTHONDONTWRITEBYTECODE",
        "1",
        "--setenv",
        "PYTHONPATH",
        "/work",
        "--tmpfs",
        "/tmp",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]
    .map(str::to_owned)
    .to_vec();
    for directory in ["/usr", "/bin", "/lib", "/lib64"] {
        if Path::new(directory).exists() {
            argv.extend(["--ro-bind", directory, directory].map(str::to_owned));
        }
    }
    argv.extend(
        [
            "--ro-bind",
            &workspace.to_string_lossy(),
            "/work",
            "--chdir",
            "/work",
        ]
        .map(str::to_owned),
    );
    argv.extend(
        [
            python,
            "-m",
            "unittest",
            "discover",
            "-s",
            "tests",
            "-p",
            "test_*.py",
        ]
        .map(str::to_owned),
    );
    argv
}

fn validate_checkpoint(
    checkpoint: &Path,
    workspace: &Path,
) -> Result<Map<String, Value>, WorkflowError> {
    let (record, _) = load(checkpoint)?;
    let flag = |name: &str, expected: bool| record.get(name) == Some(&Value::Bool(expected));
    let text = |name: &str| record.get(name).and_then(Value::as_str);
    require(
        text("mode") == Some("integrity-checkpoint"),
        "Expected integrity checkpoint",
    )?;
    require(
        flag("integrity_verified", true),
        "Integrity checkpoint not verified",
    )?;
    require(
        flag("functional_verified", false),
        "Functional verification already recorded",
    )?;
    require(
        flag("tests_executed", false),
        "Unexpected prior test execution",
    )?;
    require(
        flag("human_decision_pending", true),
        "Task not pending decision",
    )?;
    require(
        text("workspace") == Some(workspace.to_string_lossy().as_ref()),
        "Worktree disagrees with checkpoint",
    )?;
    require(
        text("next_stage") == Some("requires-separate-functional-execution-authorization"),
        "Unexpected lifecycle stage",
    )?;
    Ok(record)
}

fn is_lower_hex_40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Revalidate Git identity, ownership and the complete reviewed change set.
///
/// # Errors
/// The checkpoint identity is invalid, Git state or file contents differ from the reviewed
/// checkpoint, or an ignored Python test module is present.
pub fn audit_checkout(
    root: &Path,
    record: &Map<String, Value>,
) -> Result<BTreeMap<String, String>, WorkflowError> {
    let baseline = match record.get("baseline").and_then(Value::as_str) {
        Some(value) if is_lower_hex_40(value) => value,
        _ => return Err(WorkflowError::msg("Checkpoint Git baseline invalid")),
    };
    let task_id = record.get("task_id").and_then(Value::as_str);
    let branch = record.get("branch");
    let Some(task_id) = task_id.filter(|id| branch == Some(&json!(format!("harness/{id}")))) else {
        return Err(WorkflowError::msg("Checkpoint Git branch invalid"));
    };
    let _ = task_id;
    let files = files(record)?;
    let names: Vec<String> = files.keys().cloned().collect();
    let audit_failed = || WorkflowError::msg("Worktree Git audit failed");
    let actual_branch =
        git(root, &["symbolic-ref", "--short", "HEAD"]).map_err(|_| audit_failed())?;
    let audit = audit_worktree(root, baseline, &names).map_err(|_| audit_failed())?;
    if Some(actual_branch.as_str()) != branch.and_then(Value::as_str)
        || audit.head != baseline
        || audit.baseline != baseline
    {
        return Err(WorkflowError::msg(
            "Worktree Git identity changed since checkpoint",
        ));
    }
    let mut changed = audit.changed_paths;
    changed.sort();
    changed.dedup();
    require(
        changed == names,
        "Worktree changed paths differ from reviewed checkpoint",
    )?;
    for (name, digest) in &files {
        let target = root.join(name);
        let inside = target.is_file()
            && !target.is_symlink_lexical()
            && resolve_lenient(&target).starts_with(root);
        require(inside, "Reviewed file is absent or escapes worktree")?;
        require(
            sha256_hex(&fs::read(&target)?) == *digest,
            "Reviewed file changed since checkpoint",
        )?;
    }
    // Git's untracked-file audit ignores files hidden by .gitignore; reject ignored tests.
    let ignored = git(
        root,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
            "--",
            "tests",
        ],
    )
    .map_err(|_| WorkflowError::msg("Could not inspect ignored test files"))?;
    require(
        !ignored
            .split('\0')
            .any(|name| !name.is_empty() && is_python_name(name)),
        "Unreviewed ignored Python test file",
    )?;
    Ok(files)
}

fn write_evidence(evidence: &Path, result: &Value) -> Result<String, WorkflowError> {
    require(
        !evidence.is_symlink_lexical() && !evidence.exists(),
        "Evidence path is already occupied",
    )?;
    let parent = evidence.parent().unwrap_or_else(|| Path::new(""));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    require(
        parent.is_dir() && !parent.is_symlink_lexical(),
        "Evidence directory is invalid",
    )?;
    let payload = format!("{}\n", pretty_json_value(result)?).into_bytes();
    require(
        payload.len() <= MAX_RECEIPT,
        "Evidence receipt exceeds limit",
    )?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(
            i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).map_err(std::io::Error::other)?,
        );
    }
    options.open(evidence)?.write_all(&payload)?;
    Ok(sha256_hex(&payload))
}

/// Raw capture of one limited sandbox run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRun {
    /// Exit code, negative for a signal; `None` after a timeout.
    pub code: Option<i32>,
    /// Whether the wall-clock budget expired.
    pub timed_out: bool,
    /// Captured standard output, at most 16 KiB.
    pub stdout: Vec<u8>,
    /// Captured standard error, at most 16 KiB.
    pub stderr: Vec<u8>,
}

/// Host effects used by the functional verification gate.
///
/// Implement this trait to supply host operations to [`functional_verify_with`].
pub trait VerifyHost {
    /// Locate the Bubblewrap executable (Python `shutil.which('bwrap')`).
    fn find_bwrap(&self) -> Option<PathBuf>;
    /// Whether the sandbox interpreter exists (Python `Path(python).is_file()`).
    fn python_available(&self, python: &str) -> bool;
    /// Revalidate the checkout (Python `_audit_checkout`).
    ///
    /// # Errors
    /// Same as [`audit_checkout`].
    fn audit(
        &self,
        root: &Path,
        record: &Map<String, Value>,
    ) -> Result<BTreeMap<String, String>, WorkflowError>;
    /// Run the fixed sandbox argv under resource limits.
    ///
    /// # Errors
    /// The limiter is unavailable or the process cannot be run.
    fn run(&self, argv: &[String]) -> Result<SandboxRun, WorkflowError>;
}

/// The real host: `PATH` lookup, Git audit and `prlimit`-limited execution.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHost;

impl VerifyHost for SystemHost {
    fn find_bwrap(&self) -> Option<PathBuf> {
        which("bwrap")
    }

    fn python_available(&self, python: &str) -> bool {
        Path::new(python).is_file()
    }

    fn audit(
        &self,
        root: &Path,
        record: &Map<String, Value>,
    ) -> Result<BTreeMap<String, String>, WorkflowError> {
        audit_checkout(root, record)
    }

    fn run(&self, argv: &[String]) -> Result<SandboxRun, WorkflowError> {
        run_limited(argv)
    }
}

/// Run `argv` under `prlimit` (file size, CPU, core, address space); no host fallback.
///
/// `unsafe` is forbidden, so Python's `preexec_fn=setrlimit` is replaced by util-linux
/// `prlimit` applying the identical limits before it execs the command.
#[cfg(unix)]
fn run_limited(argv: &[String]) -> Result<SandboxRun, WorkflowError> {
    let captured = crate::multiagent::sandbox_tools::run_limited_with(
        argv,
        "hh-fv-",
        MAX_OUTPUT,
        TIMEOUT_SECONDS,
        &[
            &format!("--fsize={MAX_OUTPUT}"),
            &format!("--cpu={TIMEOUT_SECONDS}"),
            "--core=0",
            &format!("--as={}", 1024_u64 * 1024 * 1024),
        ],
    )?;
    Ok(SandboxRun {
        code: captured.code,
        timed_out: captured.timed_out,
        stdout: captured.stdout,
        stderr: captured.stderr,
    })
}

#[cfg(not(unix))]
fn run_limited(_argv: &[String]) -> Result<SandboxRun, WorkflowError> {
    Err(WorkflowError::msg(
        "Bubblewrap not installed: refusing host execution",
    ))
}

fn reviewed_tests_present(root: &Path) -> Result<(), WorkflowError> {
    let tests_dir = root.join("tests");
    require(
        tests_dir.is_dir() && !tests_dir.is_symlink_lexical(),
        "Worktree has no tests directory",
    )?;
    let tests: Vec<PathBuf> = fs::read_dir(&tests_dir)?
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("test_") && name.ends_with(".py")
        })
        .map(|entry| entry.path())
        .collect();
    require(!tests.is_empty(), "No unittest test_*.py files found")?;
    require(
        tests.iter().all(|t| t.is_file() && !t.is_symlink_lexical()),
        "Symlink or invalid test file",
    )
}

fn missing(record: &Map<String, Value>, name: &str) -> Result<Value, WorkflowError> {
    record
        .get(name)
        .cloned()
        .ok_or_else(|| WorkflowError::Message(format!("'{name}'")))
}

/// Run bounded stdlib unittest discovery with no host execution fallback.
///
/// Returns the evidence record with its `evidence` path and `evidence_sha256`.
///
/// # Errors
/// Missing authorization, a checkpoint or worktree that disagree, no Bubblewrap, no reviewed
/// tests, or an evidence destination that is not a new file within a real directory.
pub fn functional_verify(
    checkpoint: &Path,
    workspace: &Path,
    evidence: &Path,
    authorized: bool,
) -> Result<Value, WorkflowError> {
    functional_verify_with(&SystemHost, checkpoint, workspace, evidence, authorized)
}

/// Run [`functional_verify`] with caller-supplied host effects.
///
/// # Errors
/// Same as [`functional_verify`].
pub fn functional_verify_with(
    host: &dyn VerifyHost,
    checkpoint: &Path,
    workspace: &Path,
    evidence: &Path,
    authorized: bool,
) -> Result<Value, WorkflowError> {
    require(
        authorized,
        "Separate explicit functional execution authorization required",
    )?;
    require(
        workspace.is_dir() && !workspace.is_symlink_lexical(),
        "Worktree must be a real directory",
    )?;
    let root = resolve_strict(workspace)?;
    let record = validate_checkpoint(checkpoint, &root)?;
    require(
        !evidence.exists() && !evidence.is_symlink_lexical(),
        "Evidence already exists",
    )?;
    let parent = evidence.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    require(
        parent.is_dir() && !parent.is_symlink_lexical(),
        "Invalid evidence directory",
    )?;
    let Some(bwrap) = host.find_bwrap() else {
        return Err(WorkflowError::msg(
            "Bubblewrap not installed: refusing host execution",
        ));
    };
    require(host.python_available(PYTHON), "Sandbox Python unavailable")?;
    reviewed_tests_present(&root)?;
    host.audit(&root, &record)?;
    let argv = sandbox_argv(&bwrap.to_string_lossy(), &root, PYTHON);
    let run = host.run(&argv)?;
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();
    let sandbox_started =
        !stderr.starts_with("bwrap:") && !stderr.starts_with("bubblewrap:") && !run.timed_out;
    let tests_executed = sandbox_started
        && stderr.contains("Ran ")
        && (stderr.contains(" test in ") || stderr.contains(" tests in "))
        && !stderr.contains("Ran 0 tests");
    let passed = tests_executed && run.code == Some(0);
    let next_stage = if passed {
        "await-human-review"
    } else if tests_executed {
        "verification-failed-requires-review"
    } else {
        "execution-unconfirmed-requires-review"
    };
    let result = json!({
        "mode": "sandboxed-functional-verification",
        "task_id": missing(&record, "task_id")?,
        "baseline": missing(&record, "baseline")?,
        "workspace": root.to_string_lossy(),
        "checkpoint_sha256": sha256_hex(&fs::read(checkpoint)?),
        "sandbox": "bubblewrap-unshare-all-no-network",
        "command": ["python3", "-m", "unittest", "discover", "-s", "tests", "-p", "test_*.py"],
        "tests_executed": tests_executed,
        "generated_code_executed": tests_executed,
        "execution_attempted": true,
        "sandbox_started": sandbox_started,
        "functional_verified": passed,
        "exit_code": run.code,
        "timed_out": run.timed_out,
        "stdout": stdout,
        "stderr": stderr,
        "human_decision_pending": true,
        "committed": false,
        "merged": false,
        "next_stage": next_stage,
    });
    let digest = write_evidence(evidence, &result)?;
    let mut output = result;
    if let Value::Object(map) = &mut output {
        map.insert("evidence".to_owned(), json!(py_path_str(evidence)));
        map.insert("evidence_sha256".to_owned(), json!(digest));
    }
    Ok(output)
}

/// Python `str.endswith('.py')` is case-sensitive; keep that parity.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_python_name(name: &str) -> bool {
    name.ends_with(".py")
}
