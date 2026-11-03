#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic
)]

use hermes_harness::core::ownership::{check_paths, check_symlink_boundaries};
use hermes_harness::core::routing::{Role, select};

#[test]
fn cyber_disabled() {
    assert!(select(Role::Cyber, true).is_err());
}

#[test]
fn ownership() {
    let dir = tempfile::tempdir().unwrap();
    check_paths(&["src/pkg/main.py"], &["src/pkg"]).unwrap();
    assert!(check_paths(&["secrets/a.env"], &["src/pkg"]).is_err());
    assert!(check_paths(&["../outside"], &["src"]).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc", dir.path().join("link")).unwrap();
        assert!(check_symlink_boundaries(dir.path(), &["link/passwd"]).is_err());
    }
    #[cfg(not(unix))]
    check_symlink_boundaries(dir.path(), &["a/b"]).unwrap();
}
