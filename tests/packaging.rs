#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

const ARCH: &str = include_str!("../packaging/arch/PKGBUILD");
const DEBIAN: &str = include_str!("../packaging/debian/build-deb.sh");

#[test]
fn sandbox_search_dependency_is_packaged() {
    assert!(
        ARCH.lines()
            .any(|line| line.starts_with("depends=(") && line.contains("'ripgrep'"))
    );
    assert!(
        DEBIAN.lines().any(
            |line| line.starts_with("Depends:") && line.split(", ").any(|dep| dep == "ripgrep")
        )
    );
}

#[cfg(unix)]
#[test]
fn arch_version_includes_cargo_version_before_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("hermes-harness");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(
        source.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"2.3.4\"\n[dependencies]\n",
    )
    .unwrap();
    let script = format!(
        "{ARCH}\ngit() {{ case \"$1\" in rev-list) printf 42 ;; rev-parse) printf abc123 ;; *) return 1 ;; esac; }}\npkgver\n"
    );
    let output = std::process::Command::new("bash")
        .args(["-c", &script])
        .env("srcdir", tmp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "2.3.4.r42.abc123"
    );
}
