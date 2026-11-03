"""Model file proposal validation and ownership tests."""

import json

import pytest

from hermes_harness.multiagent.model_edits import parse_edit_proposal
from hermes_harness.multiagent.runtime import WorkflowError

SHA256_HEX_LENGTH = 64


def _payload(path: str) -> str:
    return json.dumps({
        'task_id': 'ticket-a',
        'files': [{'path': path, 'content': 'def test_ok(): pass\n'}],
    })


def test_accepted_proposal_is_data_only() -> None:
    proposal = parse_edit_proposal(_payload('tests/test_a.py'), 'ticket-a', ('tests',))
    assert proposal.files[0].path == 'tests/test_a.py'
    assert len(proposal.files[0].sha256) == SHA256_HEX_LENGTH


@pytest.mark.parametrize(
    'path',
    [
        '../escape',
        '/etc/passwd',
        '.git/config',
        'src/unowned.py',
        'tests/../other.py',
        'tests//bad.py',
    ],
)
def test_rejects_unowned_or_unsafe_paths(path: str) -> None:
    with pytest.raises(WorkflowError, match='forbidden or unowned'):
        parse_edit_proposal(_payload(path), 'ticket-a', ('tests',))


def test_identity_and_duplicate_paths_fail() -> None:
    with pytest.raises(WorkflowError, match='identity mismatch'):
        parse_edit_proposal(_payload('tests/a.py'), 'other', ('tests',))
    duplicate = json.dumps({
        'task_id': 'ticket-a',
        'files': [
            {'path': 'tests/a.py', 'content': 'a'},
            {'path': 'tests/a.py', 'content': 'b'},
        ],
    })
    with pytest.raises(WorkflowError, match='Duplicate'):
        parse_edit_proposal(duplicate, 'ticket-a', ('tests',))
