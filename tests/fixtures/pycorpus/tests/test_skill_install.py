"""Dry-run Hermes Harness skill install and collision rejection."""

from typing import TYPE_CHECKING

import pytest

from hermes_harness.native.skill_install import install_skills, prepare_skills

if TYPE_CHECKING:
    from pathlib import Path

EXPECTED_SKILL_COUNT = 5


def _source(tmp_path: Path) -> tuple[Path, Path]:
    source = tmp_path / 'source'
    target = tmp_path / 'skills'
    target.mkdir()
    for name in (
        'lets-investigate',
        'game-plan',
        'agents-assemble',
        'what-we-know',
        'cyber-warrior',
    ):
        folder = source / name
        folder.mkdir(parents=True)
        (folder / 'SKILL.md').write_text('---\nname: ' + name + '\n---\n')
    return source, target


def test_discovery_and_install_to_nested_category(tmp_path: Path) -> None:
    source, target = _source(tmp_path)
    manifests = prepare_skills(source, target)
    assert len(manifests) == EXPECTED_SKILL_COUNT
    assert not (target / 'hermes-harness').exists()
    install_skills(manifests)
    assert (target / 'hermes-harness/game-plan/SKILL.md').is_file()
    with pytest.raises(ValueError, match='overwrite'):
        prepare_skills(source, target)


def test_rejects_symlinked_skill_source(tmp_path: Path) -> None:
    source, target = _source(tmp_path)
    manifest = source / 'game-plan/SKILL.md'
    manifest.unlink()
    manifest.symlink_to(source / 'lets-investigate/SKILL.md')
    with pytest.raises(ValueError, match='symlinked'):
        prepare_skills(source, target)


def test_refuses_existing_category(tmp_path: Path) -> None:
    source, target = _source(tmp_path)
    (target / 'hermes-harness').mkdir()
    with pytest.raises(ValueError, match='overwrite'):
        prepare_skills(source, target)
