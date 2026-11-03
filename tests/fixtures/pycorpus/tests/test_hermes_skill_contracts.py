"""Hermes v0.22.5 skill manifest contract tests.

No external YAML dependency: validate the fixed, trusted frontmatter format
we generate for the five harness skills.
"""

from pathlib import Path

import pytest

from hermes_harness.native.skill_install import SKILLS

SKILL_ROOT = Path(__file__).resolve().parents[1] / 'hermes_skills'

MAX_SKILL_DESCRIPTION_LENGTH = 60

MAX_SKILL_FILE_BYTES = 100_000


@pytest.mark.parametrize('skill', SKILLS)
def test_skill_manifest_contract(skill: str) -> None:
    path = SKILL_ROOT / skill / 'SKILL.md'
    content = path.read_text(encoding='utf-8')
    assert content.startswith('---\n')
    frontmatter, body = content[4:].split('\n---\n', 1)
    assert f'name: {skill}' in frontmatter.splitlines()
    descriptions = [
        line.removeprefix('description: ')
        for line in frontmatter.splitlines()
        if line.startswith('description: ')
    ]
    assert len(descriptions) == 1
    assert 0 < len(descriptions[0]) <= MAX_SKILL_DESCRIPTION_LENGTH
    assert descriptions[0].endswith('.')
    assert 'platforms: [linux]' in frontmatter
    assert 'author: ' in frontmatter
    assert 'license: ' in frontmatter
    assert 'metadata:' in frontmatter
    assert '## When to Use' in body
    assert '## Pitfalls' in body
    assert '## Verification' in body
    assert len(content) <= MAX_SKILL_FILE_BYTES
