//! Pure, fail-closed checks for supervised sandbox argv and service recovery.
//!
//! No privileged commands, provider calls, or mutation of a service ledger.

use std::collections::BTreeSet;

/// A proposed sandbox does not establish its claimed role containment.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OperatorSafetyError(String);

fn refuse<T>(message: &str) -> Result<T, OperatorSafetyError> {
    Err(OperatorSafetyError(message.to_owned()))
}

/// A `(source, destination)` mount pair.
pub type MountPair = (String, String);

/// Exact role mount sources and destinations extracted from argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEvidence {
    /// Writable binds (`--bind`, `--dev-bind`).
    pub writable: Vec<MountPair>,
    /// Read-only binds (`--ro-bind`).
    pub readonly: Vec<MountPair>,
}

const MOUNT_FLAGS: [&str; 3] = ["--bind", "--ro-bind", "--dev-bind"];
const REQUIRED_FLAGS: [&str; 4] = [
    "--unshare-net",
    "--unshare-pid",
    "--unshare-user",
    "--die-with-parent",
];
const BWRAP: &str = "/usr/bin/bwrap";

fn mount_decl(argv: &[String], cursor: usize) -> Result<MountPair, OperatorSafetyError> {
    let (Some(source), Some(destination)) = (argv.get(cursor + 1), argv.get(cursor + 2)) else {
        return refuse("Incomplete bubblewrap mount declaration");
    };
    if !source.starts_with('/') || !destination.starts_with('/') {
        return refuse("Mount must use absolute source and destination");
    }
    Ok((source.clone(), destination.clone()))
}

fn bwrap_index(argv: &[String]) -> Result<usize, OperatorSafetyError> {
    let mut found = argv.iter().enumerate().filter(|(_, arg)| *arg == BWRAP);
    match (found.next(), found.next()) {
        (Some((index, _)), None) => Ok(index),
        _ => refuse("Exactly one explicit bubblewrap executable required"),
    }
}

/// Parse bwrap mount triples without interpreting payload arguments as options.
///
/// # Errors
/// The mount evidence has an invalid shape or unsupported mount values.
pub fn extract_mounts(argv: &[String]) -> Result<MountEvidence, OperatorSafetyError> {
    let mut cursor = bwrap_index(argv)? + 1;
    let mut writable = Vec::new();
    let mut readonly = Vec::new();
    while let Some(option) = argv.get(cursor) {
        if option == "--chdir" {
            break;
        }
        if !MOUNT_FLAGS.contains(&option.as_str()) {
            cursor += 1;
            continue;
        }
        let pair = mount_decl(argv, cursor)?;
        if option == "--ro-bind" {
            readonly.push(pair);
        } else {
            writable.push(pair);
        }
        cursor += 3;
    }
    Ok(MountEvidence { writable, readonly })
}

fn at<'a>(pairs: &'a [MountPair], destination: &'a str) -> impl Iterator<Item = &'a MountPair> {
    pairs.iter().filter(move |pair| pair.1 == destination)
}

fn check_role_mounts(mounts: &MountEvidence, role: &str) -> Result<(), OperatorSafetyError> {
    let work_writable = at(&mounts.writable, "/workspace").count();
    let work_readonly = at(&mounts.readonly, "/workspace").count();
    let evidence_readonly = at(&mounts.readonly, "/evidence").count();
    if evidence_readonly != 1 || at(&mounts.writable, "/evidence").next().is_some() {
        return refuse("Evidence must have one read-only source");
    }
    if role == "worker" {
        if work_writable != 1 || work_readonly != 0 {
            return refuse("Worker workspace must have one writable source");
        }
    } else if work_readonly != 1 || work_writable != 0 || !mounts.writable.is_empty() {
        return refuse("Reviewer must have one read-only workspace and no writable bind mounts");
    }
    Ok(())
}

/// Require role-specific mounts and namespace isolation before role admission.
///
/// # Errors
/// Mount evidence is malformed or violates role isolation requirements.
pub fn validate_role_sandbox(
    argv: &[String],
    role: &str,
) -> Result<MountEvidence, OperatorSafetyError> {
    if !matches!(role, "worker" | "reviewer") {
        return refuse("Unknown sandbox role");
    }
    let mounts = extract_mounts(argv)?;
    let mut options = argv.get(bwrap_index(argv)? + 1..).unwrap_or_default();
    if let Some(chdir) = options.iter().position(|arg| arg == "--chdir") {
        options = options.get(..chdir).unwrap_or_default();
    }
    if REQUIRED_FLAGS
        .iter()
        .any(|flag| !options.iter().any(|arg| arg == flag))
    {
        return refuse("Required sandbox isolation missing");
    }
    check_role_mounts(&mounts, role)?;
    let sources: Vec<&str> = mounts
        .writable
        .iter()
        .chain(&mounts.readonly)
        .map(|(source, _)| source.as_str())
        .collect();
    if sources.len() != sources.iter().collect::<BTreeSet<_>>().len() {
        return refuse("Mount sources must not be reused across sandbox destinations");
    }
    Ok(mounts)
}

/// Ensure reviewer never mounts the worker's writable source or shared evidence as writable.
///
/// # Errors
/// Mount evidence is malformed or violates role isolation requirements.
pub fn validate_separate_roles(
    worker_argv: &[String],
    reviewer_argv: &[String],
) -> Result<(), OperatorSafetyError> {
    let worker = validate_role_sandbox(worker_argv, "worker")?;
    let reviewer = validate_role_sandbox(reviewer_argv, "reviewer")?;
    let aliased = worker.writable.first().is_some_and(|(writable, _)| {
        reviewer
            .readonly
            .iter()
            .any(|(source, _)| source == writable)
    });
    if aliased {
        return refuse("Reviewer must not receive the worker writable workspace");
    }
    Ok(())
}
