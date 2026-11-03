//! Explicitly stage Hermes Harness skill manifests under a category directory.
//!
//! No installation is performed without `--apply`. Refuses overwrite, symlinks, and
//! unexpected source files. Intended to be run as the restricted hermes account.

use crate::util::PathLexical as _;
use crate::util::create_private_dir;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Parser;

/// Skill names shipped by Hermes Harness, in install order.
pub const SKILLS: [&str; 5] = [
    "lets-investigate",
    "game-plan",
    "agents-assemble",
    "what-we-know",
    "cyber-warrior",
];
/// Category directory created under the Hermes skills root.
pub const CATEGORY: &str = "hermes-harness";
const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// Refusal or filesystem failure while staging skills (Python `ValueError` / `OSError`).
#[derive(Debug, thiserror::Error)]
pub enum SkillInstallError {
    /// Rule violation.
    #[error("{0}")]
    Invalid(String),
    /// Filesystem failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

fn invalid<T>(message: impl Into<String>) -> Result<T, SkillInstallError> {
    Err(SkillInstallError::Invalid(message.into()))
}

/// One planned manifest copy.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SkillInstall {
    /// Skill name.
    pub name: String,
    /// Source `SKILL.md`.
    pub source: PathBuf,
    /// Destination `SKILL.md`.
    pub destination: PathBuf,
}

/// Inspect every known source and destination before any mutation.
///
/// # Errors
/// A root is not a real directory, the category already exists, or a manifest is missing,
/// symlinked, empty or oversized.
pub fn prepare_skills(
    source_root: &Path,
    target_root: &Path,
) -> Result<Vec<SkillInstall>, SkillInstallError> {
    if source_root.is_symlink_lexical() || !source_root.is_dir() {
        return invalid("Skill source root must be a real directory");
    }
    if target_root.is_symlink_lexical() || !target_root.is_dir() {
        return invalid("Existing Hermes skills root must be a real directory");
    }
    let category = target_root.join(CATEGORY);
    if category.exists() || category.is_symlink_lexical() {
        return invalid("Refusing to overwrite existing Hermes Harness skill category");
    }
    let mut manifests = Vec::with_capacity(SKILLS.len());
    for name in SKILLS {
        let source = source_root.join(name).join("SKILL.md");
        if source.is_symlink_lexical() || !source.is_file() {
            return invalid(format!("Missing or symlinked skill manifest: {name}"));
        }
        let raw = fs::read(&source)?;
        if raw.is_empty() || raw.len() > MAX_MANIFEST_BYTES {
            return invalid(format!("Invalid or oversized skill manifest: {name}"));
        }
        manifests.push(SkillInstall {
            name: name.to_owned(),
            source,
            destination: category.join(name).join("SKILL.md"),
        });
    }
    Ok(manifests)
}

/// Install a new category exclusively; never replace an existing category.
///
/// # Errors
/// No manifests were supplied, or a directory or file already exists or cannot be created.
pub fn install_skills(manifests: &[SkillInstall]) -> Result<(), SkillInstallError> {
    let Some(first) = manifests.first() else {
        return invalid("No skill manifests supplied");
    };
    let category = first
        .destination
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| SkillInstallError::Invalid("Invalid skill destination".to_owned()))?;
    create_private_dir(category)?;
    for manifest in manifests {
        if let Some(directory) = manifest.destination.parent() {
            create_private_dir(directory)?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options
            .open(&manifest.destination)?
            .write_all(&fs::read(&manifest.source)?)?;
    }
    Ok(())
}

/// Command-line arguments of the skill installer.
#[derive(Debug, Clone, PartialEq, Eq, Parser)]
#[command(args_override_self = true, infer_long_args = true)]
pub struct SkillInstallArgs {
    /// Directory holding one folder per skill.
    #[arg(long)]
    pub source: PathBuf,
    /// Existing Hermes skills root.
    #[arg(long)]
    pub target: PathBuf,
    /// Actually install; without it only a dry run is reported.
    #[arg(long)]
    pub apply: bool,
}

/// Dry-run by default; installation always requires explicit `--apply`.
///
/// Returns the text Python prints to standard output.
///
/// # Errors
/// Any failure of [`prepare_skills`] or [`install_skills`].
pub fn run(arguments: &SkillInstallArgs) -> Result<String, SkillInstallError> {
    let mut output = Vec::new();
    run_with(arguments, &mut output)?;
    String::from_utf8(output).map_err(|error| SkillInstallError::Invalid(error.to_string()))
}

/// Print mappings before installation so they survive an installation failure.
///
/// # Errors
/// Preparation, output, or installation fails.
pub fn run_with(
    arguments: &SkillInstallArgs,
    output: &mut dyn Write,
) -> Result<(), SkillInstallError> {
    let manifests = prepare_skills(&arguments.source, &arguments.target)?;
    for item in &manifests {
        writeln!(
            output,
            "{} -> {}",
            item.source.display(),
            item.destination.display()
        )?;
    }
    output.flush()?;
    if arguments.apply {
        install_skills(&manifests)?;
        writeln!(
            output,
            "Installed Hermes Harness skill manifests; start a fresh Hermes session."
        )?;
    } else {
        writeln!(
            output,
            "DRY RUN: no skill files changed. Supply --apply after reviewing."
        )?;
    }
    output.flush()?;
    Ok(())
}
