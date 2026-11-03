#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    clippy::nursery
)]
//! Dry-run Hermes Harness skill install and collision rejection.

use std::path::{Path, PathBuf};

use hermes_harness::native::skill_install::{
    SkillInstallArgs, install_skills, prepare_skills, run_with,
};

const EXPECTED_SKILL_COUNT: usize = 5;

#[test]
fn mapping_lines_survive_install_failure() {
    use std::io::{self, Write};

    struct RaceOnFlush {
        output: Vec<u8>,
        category: PathBuf,
    }

    impl Write for RaceOnFlush {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            std::fs::create_dir(&self.category)
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let (source, target) = source(tmp.path());
    let manifests = prepare_skills(&source, &target).unwrap();
    let expected: String = manifests
        .iter()
        .map(|item| {
            format!(
                "{} -> {}\n",
                item.source.display(),
                item.destination.display()
            )
        })
        .collect();
    let mut output = RaceOnFlush {
        output: Vec::new(),
        category: target.join("hermes-harness"),
    };
    let error = run_with(
        &SkillInstallArgs {
            source,
            target,
            apply: true,
        },
        &mut output,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        hermes_harness::native::skill_install::SkillInstallError::Io(_)
    ));
    assert_eq!(output.output, expected.as_bytes());
}

fn source(tmp: &Path) -> (PathBuf, PathBuf) {
    let source = tmp.join("source");
    let target = tmp.join("skills");
    std::fs::create_dir(&target).unwrap();
    for name in [
        "lets-investigate",
        "game-plan",
        "agents-assemble",
        "what-we-know",
        "cyber-warrior",
    ] {
        let folder = source.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("SKILL.md"), format!("---\nname: {name}\n---\n")).unwrap();
    }
    (source, target)
}

#[test]
fn discovery_and_install_to_nested_category() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, target) = source(tmp.path());
    let manifests = prepare_skills(&source, &target).unwrap();
    assert_eq!(manifests.len(), EXPECTED_SKILL_COUNT);
    assert!(!target.join("hermes-harness").exists());
    install_skills(&manifests).unwrap();
    assert!(target.join("hermes-harness/game-plan/SKILL.md").is_file());
    let error = prepare_skills(&source, &target).unwrap_err();
    assert!(error.to_string().contains("overwrite"), "{error}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_skill_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, target) = source(tmp.path());
    let manifest = source.join("game-plan/SKILL.md");
    std::fs::remove_file(&manifest).unwrap();
    std::os::unix::fs::symlink(source.join("lets-investigate/SKILL.md"), &manifest).unwrap();
    let error = prepare_skills(&source, &target).unwrap_err();
    assert!(error.to_string().contains("symlinked"), "{error}");
}

#[test]
fn refuses_existing_category() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, target) = source(tmp.path());
    std::fs::create_dir(target.join("hermes-harness")).unwrap();
    let error = prepare_skills(&source, &target).unwrap_err();
    assert!(error.to_string().contains("overwrite"), "{error}");
}

#[test]
fn symlinked_roots_with_trailing_slash_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, target) = source(tmp.path());
    let link = tmp.path().join("link");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&source, &link).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(&source, &link).is_ok();
    if !made {
        return;
    }
    let slashed = PathBuf::from(format!("{}/", link.display()));
    let error = prepare_skills(&slashed, &target).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Skill source root must be a real directory"
    );
}
