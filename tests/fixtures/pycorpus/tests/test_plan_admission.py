"""Plan admission regression tests for structured coordinator output."""

import json

import pytest

from hermes_harness.multiagent.plan_io import parse_plan_json
from hermes_harness.multiagent.runtime import WorkflowError


def _plan() -> dict[str, object]:
    return {
        'id': 'ticket-1',
        'objective': 'Improve tests',
        'nodes': [
            {'id': 'tests', 'objective': 'Add regression test', 'owned_paths': ['tests/new.py']},
            {
                'id': 'docs',
                'objective': 'Document behavior',
                'owned_paths': ['docs/new.md'],
                'dependencies': ['tests'],
            },
        ],
    }


def test_accepted_plan_retains_dependencies() -> None:
    result = parse_plan_json(json.dumps(_plan()))
    assert result.nodes[1].dependencies == ('tests',)
    assert result.nodes[0].role == 'coding'


@pytest.mark.parametrize(
    'payload',
    [
        '{}',
        '{"id":"x","id":"y"}',
        '{"id":true,"objective":"x","nodes":[]}',
        '{"id":"x","objective":"x","nodes":[],"unauthorized":true}',
        'not json',
    ],
)
def test_invalid_document_is_rejected(payload: str) -> None:
    with pytest.raises(WorkflowError):
        parse_plan_json(payload)


def test_unsupported_role_rejected() -> None:
    data = _plan()
    nodes = data['nodes']
    assert isinstance(nodes, list)
    nodes[0]['role'] = 'root'
    with pytest.raises(WorkflowError, match='Unknown worker role'):
        parse_plan_json(json.dumps(data))


def test_oversized_payload_rejected() -> None:
    with pytest.raises(WorkflowError, match='byte limit'):
        parse_plan_json(' ' * 65537)


def test_ownership_overlap_rejected() -> None:
    data = _plan()
    nodes = data['nodes']
    assert isinstance(nodes, list)
    nodes[1]['owned_paths'] = ['tests/new.py']
    with pytest.raises(WorkflowError, match='Overlapping'):
        parse_plan_json(json.dumps(data))
