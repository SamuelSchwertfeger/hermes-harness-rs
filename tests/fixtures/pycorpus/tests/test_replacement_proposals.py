"""Schema/ownership validation for local model replacement proposals."""

import hashlib
import json

import pytest

from hermes_harness.multiagent.replacement_proposals import parse_replacement_proposal
from hermes_harness.multiagent.runtime import WorkflowError


def _payload(path: str = 'src/a.py') -> str:
    return json.dumps({
        'task_id': 'task-a',
        'replacements': [
            {
                'path': path,
                'expected_sha256': hashlib.sha256(b'old').hexdigest(),
                'content': 'new',
            }
        ],
    })


def test_parse_scoped_replacement() -> None:
    result = parse_replacement_proposal(_payload(), 'task-a', ('src',))
    assert result[0].path == 'src/a.py'
    assert result[0].content == 'new'


@pytest.mark.parametrize('path', ['../escape', '/tmp/x', '.git/config', 'other.py', 'src/../x'])  # noqa: S108
def test_rejects_unsafe_path(path: str) -> None:
    with pytest.raises(WorkflowError):
        parse_replacement_proposal(_payload(path), 'task-a', ('src',))


def test_rejects_mismatched_task() -> None:
    with pytest.raises(WorkflowError, match='task mismatch'):
        parse_replacement_proposal(_payload(), 'task-b', ('src',))


def test_rejects_duplicate_json_fields() -> None:
    with pytest.raises(WorkflowError, match='Duplicate'):
        parse_replacement_proposal('{"task_id":"x","task_id":"x"}', 'x', ('src',))
