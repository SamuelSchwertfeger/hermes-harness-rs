"""Explicitly stage Hermes Harness skill manifests under a category directory.

No installation is performed without --apply. Refuses overwrite, symlinks, and
unexpected source files. Intended to be run as the restricted hermes account.
"""

import argparse
import os
from dataclasses import dataclass
from pathlib import Path

SKILLS = (
    'lets-investigate',
    'game-plan',
    'agents-assemble',
    'what-we-know',
    'cyber-warrior',
)
CATEGORY = 'hermes-harness'


@dataclass(frozen=True, slots=True)
class SkillInstall:
    name: str
    source: Path
    destination: Path


def prepare_skills(source_root: Path, target_root: Path) -> tuple[SkillInstall, ...]:
    """Inspect every known source and destination before any mutation."""
    if source_root.is_symlink() or not source_root.is_dir():
        msg = 'Skill source root must be a real directory'
        raise ValueError(msg)
    if target_root.is_symlink() or not target_root.is_dir():
        msg = 'Existing Hermes skills root must be a real directory'
        raise ValueError(msg)
    category = target_root / CATEGORY
    if category.exists() or category.is_symlink():
        msg = 'Refusing to overwrite existing Hermes Harness skill category'
        raise ValueError(msg)
    manifests: list[SkillInstall] = []
    for name in SKILLS:
        source = source_root / name / 'SKILL.md'
        if source.is_symlink() or not source.is_file():
            msg = f'Missing or symlinked skill manifest: {name}'
            raise ValueError(msg)
        raw = source.read_bytes()
        if not raw or len(raw) > 64 * 1024:
            msg = f'Invalid or oversized skill manifest: {name}'
            raise ValueError(msg)
        manifests.append(SkillInstall(name, source, category / name / 'SKILL.md'))
    return tuple(manifests)


def install_skills(manifests: tuple[SkillInstall, ...]) -> None:
    """Install a new category exclusively; never replace an existing category."""
    if not manifests:
        msg = 'No skill manifests supplied'
        raise ValueError(msg)
    category = manifests[0].destination.parent.parent
    category.mkdir(mode=0o700, exist_ok=False)
    for manifest in manifests:
        directory = manifest.destination.parent
        directory.mkdir(mode=0o700, exist_ok=False)
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, 'O_NOFOLLOW'):
            flags |= os.O_NOFOLLOW
        with os.fdopen(os.open(manifest.destination, flags, 0o600), 'wb') as output:
            output.write(manifest.source.read_bytes())


def main() -> None:
    """Dry-run by default; installation always requires explicit --apply."""
    parser = argparse.ArgumentParser()
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--target', type=Path, required=True)
    parser.add_argument('--apply', action='store_true')
    arguments = parser.parse_args()
    manifests = prepare_skills(arguments.source, arguments.target)
    for item in manifests:
        print(f'{item.source} -> {item.destination}')
    if arguments.apply:
        install_skills(manifests)
        print('Installed Hermes Harness skill manifests; start a fresh Hermes session.')
    else:
        print('DRY RUN: no skill files changed. Supply --apply after reviewing.')


if __name__ == '__main__':
    main()
