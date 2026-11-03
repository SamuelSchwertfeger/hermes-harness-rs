//! Verify working tree changes are confined to an approved set of paths.

use std::io;
use std::path::Path;

use crate::util::resolve_lenient;

/// Errors from ownership checks (Python `PermissionError` plus I/O).
#[derive(Debug, thiserror::Error)]
pub enum OwnershipError {
    /// A path was unsafe, unowned or escaped the repository.
    #[error("{0}")]
    Permission(String),
    /// Filesystem failure while resolving paths.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Reject unsafe paths and paths outside every allowed root.
///
/// # Errors
/// A changed path is outside the owned paths or is explicitly forbidden.
pub fn check_paths<C, A>(changed: &[C], allowed: &[A]) -> Result<(), OwnershipError>
where
    C: AsRef<str>,
    A: AsRef<str>,
{
    for name in changed.iter().map(AsRef::as_ref) {
        if name.is_empty() || name.starts_with('/') || name.split('/').any(|part| part == "..") {
            return Err(OwnershipError::Permission(format!(
                "Unsafe changed path: {name}"
            )));
        }
        let owned = allowed.iter().map(AsRef::as_ref).any(|root| {
            name == root
                || name
                    .strip_prefix(root.trim_end_matches('/'))
                    .is_some_and(|rest| rest.starts_with('/'))
        });
        if !owned {
            return Err(OwnershipError::Permission(format!(
                "Unowned changed path: {name}"
            )));
        }
    }
    Ok(())
}

/// Reject any touched path routed through a symlink outside the repository.
///
/// # Errors
/// A path escapes the permitted roots or crosses a forbidden symlink boundary.
pub fn check_symlink_boundaries<C>(repo: &Path, changed: &[C]) -> Result<(), OwnershipError>
where
    C: AsRef<str>,
{
    std::fs::metadata(repo)?; // repository must exist
    let root = resolve_lenient(repo);
    for name in changed.iter().map(AsRef::as_ref) {
        if !resolve_lenient(&repo.join(name)).starts_with(&root) {
            return Err(OwnershipError::Permission(format!(
                "Changed path escapes repository: {name}"
            )));
        }
    }
    Ok(())
}
