//! Bounded, offline source scan for Hermes submission/admission lookup candidates.
//!
//! This reports source evidence only. It does not prove that a handler is registered,
//! reachable over RPC, authenticated, or safe to use for recovery.
// NOTE: Python's `ast` is replaced by the hand-written scanner in `gateway_contract_probe::pysource`.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;

use crate::util::bin_main::parse_args;

use super::gateway_contract_probe::pysource::{
    FuncDef, function_defs, identifier_hits, string_constants,
};
use crate::util::{pretty_json, resolve_strict};

/// Largest single file scanned.
pub const MAX_FILE_BYTES: u64 = 2_000_000;
/// Most Python files collected.
pub const MAX_FILES: usize = 5_000;
/// Most bytes scanned in total.
pub const MAX_TOTAL_BYTES: u64 = 50_000_000;
/// Most findings kept.
pub const MAX_FINDINGS: usize = 500;
/// Directory names never entered.
pub const SKIP_DIRS: [&str; 5] = [
    ".git",
    ".venv",
    "__pycache__",
    "node_modules",
    ".mypy_cache",
];
/// Identifier names that mark a lookup candidate.
pub const IDENTIFIER_TERMS: [&str; 3] = ["submission_id", "admission_id", "request_id"];
/// String fragments that mark a lookup candidate.
pub const STRING_TERMS: [&str; 4] = [
    "submission_id",
    "admission_id",
    "prompt.receipt",
    "prompt.submit",
];

/// Raised when the source tree cannot be inspected within configured bounds.
#[derive(Debug, thiserror::Error)]
pub enum LookupProbeError {
    /// Python `LookupProbeError`.
    #[error("{0}")]
    Probe(String),
    /// Python `ValueError` (invalid limits).
    #[error("{0}")]
    Value(String),
    /// Filesystem failure.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Resource bounds for an offline source-tree inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanLimits {
    /// Most Python files collected.
    pub max_files: usize,
    /// Largest single file scanned.
    pub max_file_bytes: u64,
    /// Most bytes scanned in total.
    pub max_total_bytes: u64,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_files: MAX_FILES,
            max_file_bytes: MAX_FILE_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
        }
    }
}

impl ScanLimits {
    /// Reject non-positive limits.
    ///
    /// # Errors
    /// A scan limit is outside the permitted bounds.
    pub fn validate(&self) -> Result<(), LookupProbeError> {
        if self.max_files < 1 || self.max_file_bytes < 1 || self.max_total_bytes < 1 {
            return Err(LookupProbeError::Value(
                "Inspection limits must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

/// One function containing static lookup-related source evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LookupFinding {
    /// Path relative to the scanned root.
    pub path: String,
    /// One-based line of the first piece of evidence.
    pub line: usize,
    /// Dotted symbol path.
    pub symbol: String,
    /// Sorted identifier names found.
    pub identifiers: Vec<String>,
    /// Sorted matching string literals (truncated to 160 characters).
    pub string_literals: Vec<String>,
}

/// A Python file that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParseFailure {
    /// Path relative to the scanned root.
    pub path: String,
    /// One-based line of the problem (0 when unknown).
    pub line: usize,
    /// Problem description.
    pub message: String,
}

/// Static scan output; findings are candidates, not verified RPC capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LookupProbeReport {
    /// Scanned root.
    pub root: String,
    /// Files actually scanned.
    pub files_scanned: usize,
    /// Bytes actually scanned.
    pub bytes_scanned: u64,
    /// Sorted findings.
    pub findings: Vec<LookupFinding>,
    /// Files that failed to parse or exceeded the size limit.
    pub parse_failures: Vec<ParseFailure>,
    /// True when a bound stopped the scan early.
    pub truncated: bool,
    /// Always false: findings are not proof.
    pub capability_verified: bool,
}

#[derive(Default)]
struct ScanState {
    findings: Vec<LookupFinding>,
    parse_failures: Vec<ParseFailure>,
    bytes_scanned: u64,
    files_scanned: usize,
    truncated: bool,
}

fn finding_for(function: &FuncDef, relative_path: &str) -> Option<LookupFinding> {
    let is_term = |name: &str| IDENTIFIER_TERMS.contains(&name);
    let mut lines: Vec<usize> = Vec::new();
    let mut identifiers = BTreeSet::new();
    for (name, line) in identifier_hits(&function.tokens, is_term) {
        identifiers.insert(name);
        lines.push(line);
    }
    let mut literals = BTreeSet::new();
    for (value, line) in string_constants(&function.tokens) {
        if STRING_TERMS.iter().any(|term| value.contains(term)) {
            literals.insert(value.chars().take(160).collect::<String>());
            lines.push(line);
        }
    }
    let named = IDENTIFIER_TERMS
        .iter()
        .any(|term| function.name.contains(term));
    if identifiers.is_empty() && literals.is_empty() && !named {
        return None;
    }
    Some(LookupFinding {
        path: relative_path.to_owned(),
        line: lines.into_iter().min().unwrap_or(function.line),
        symbol: function.qualified_name(),
        identifiers: identifiers.into_iter().collect(),
        string_literals: literals.into_iter().collect(),
    })
}

fn eligible_directory(parent: &Path, name: &str) -> bool {
    let path = parent.join(name);
    !SKIP_DIRS.contains(&name)
        && !name.starts_with('.')
        && !crate::util::lstat(&path).is_ok_and(|meta| meta.is_symlink())
}

fn append_python_path(
    paths: &mut Vec<PathBuf>,
    path: PathBuf,
    max_files: usize,
) -> Result<(), LookupProbeError> {
    if crate::util::lstat(&path).is_ok_and(|meta| meta.is_symlink()) {
        return Ok(());
    }
    paths.push(path);
    if paths.len() > max_files {
        return Err(LookupProbeError::Probe(format!(
            "Source tree exceeds file limit ({max_files})"
        )));
    }
    Ok(())
}

fn collect_paths(
    current: &Path,
    paths: &mut Vec<PathBuf>,
    max_files: usize,
) -> Result<(), LookupProbeError> {
    let Ok(entries) = fs::read_dir(current) else {
        return Ok(());
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }
    dirs.retain(|name| eligible_directory(current, name));
    dirs.sort();
    files.sort();
    for filename in files
        .iter()
        .filter(|name| name.as_bytes().ends_with(b".py"))
    {
        append_python_path(paths, current.join(filename), max_files)?;
    }
    for name in &dirs {
        collect_paths(&current.join(name), paths, max_files)?;
    }
    Ok(())
}

/// Python files under `base` in `os.walk` order, bounded by `max_files`.
///
/// # Errors
/// Scan limits are invalid or checkout files cannot be enumerated or read.
fn python_paths(base: &Path, max_files: usize) -> Result<Vec<PathBuf>, LookupProbeError> {
    let mut paths = Vec::new();
    collect_paths(base, &mut paths, max_files)?;
    Ok(paths)
}

fn relative_posix(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

type FileScan = (Vec<LookupFinding>, Option<ParseFailure>);

fn inspect_file(path: &Path, base: &Path) -> Result<FileScan, LookupProbeError> {
    let relative = relative_posix(path, base);
    let source = fs::read_to_string(path)?;
    match function_defs(&source) {
        Err(exc) => Ok((
            Vec::new(),
            Some(ParseFailure {
                path: relative,
                line: exc.line,
                message: exc.msg,
            }),
        )),
        Ok(defs) => Ok((
            defs.iter()
                .filter_map(|function| finding_for(function, &relative))
                .collect(),
            None,
        )),
    }
}

fn scan_paths(
    paths: &[PathBuf],
    base: &Path,
    limits: ScanLimits,
) -> Result<ScanState, LookupProbeError> {
    let mut state = ScanState::default();
    for path in paths {
        let size = fs::metadata(path)?.len();
        let relative = relative_posix(path, base);
        if size > limits.max_file_bytes {
            state.parse_failures.push(ParseFailure {
                path: relative,
                line: 0,
                message: "file exceeds size limit".to_owned(),
            });
            continue;
        }
        if state.bytes_scanned + size > limits.max_total_bytes {
            state.truncated = true;
            break;
        }
        let (findings, failure) = inspect_file(path, base)?;
        state.bytes_scanned += size;
        state.files_scanned += 1;
        state.findings.extend(findings);
        state.parse_failures.extend(failure);
        if state.findings.len() >= MAX_FINDINGS {
            state.findings.truncate(MAX_FINDINGS);
            state.truncated = true;
            break;
        }
    }
    Ok(state)
}

/// Scan an explicit source tree without importing or executing its Python files.
///
/// # Errors
/// Scan limits are invalid or checkout files cannot be enumerated or read.
pub fn inspect_tree(
    root: &Path,
    limits: Option<ScanLimits>,
) -> Result<LookupProbeReport, LookupProbeError> {
    let scan_limits = limits.unwrap_or_default();
    scan_limits.validate()?;
    let base = resolve_strict(root)?;
    if !base.is_dir() {
        return Err(LookupProbeError::Probe(
            "Source root must be a directory".to_owned(),
        ));
    }
    let paths = python_paths(&base, scan_limits.max_files)?;
    let mut state = scan_paths(&paths, &base, scan_limits)?;
    state
        .findings
        .sort_by(|a, b| (&a.path, a.line, &a.symbol).cmp(&(&b.path, b.line, &b.symbol)));
    Ok(LookupProbeReport {
        root: base.to_string_lossy().into_owned(),
        files_scanned: state.files_scanned,
        bytes_scanned: state.bytes_scanned,
        findings: state.findings,
        parse_failures: state.parse_failures,
        truncated: state.truncated,
        capability_verified: false,
    })
}

#[derive(Parser)]
#[command(args_override_self = true, infer_long_args = true)]
struct Args {
    source_root: PathBuf,
}

/// Command-line entry point; `args` excludes the program name. Returns the exit code.
pub fn run(args: &[impl AsRef<OsStr>], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let parsed: Args = match parse_args(
        "gateway_lookup_probe",
        args,
        "Find static Hermes submission/admission lookup candidates without execution",
        "usage: gateway_lookup_probe [-h] source_root",
        stdout,
        stderr,
    ) {
        Ok(parsed) => parsed,
        Err(status) => return status,
    };
    let outcome = inspect_tree(&parsed.source_root, None)
        .map_err(|exc| exc.to_string())
        .and_then(|report| pretty_json(&report).map_err(|exc| exc.to_string()));
    match outcome {
        Ok(text) => {
            let _ = writeln!(stdout, "{text}");
            0
        }
        Err(message) => {
            let _ = writeln!(stderr, "Lookup inspection failed: {message}");
            2
        }
    }
}
