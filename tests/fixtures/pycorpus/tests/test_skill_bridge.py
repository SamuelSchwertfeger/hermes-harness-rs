"""Hermes terminal bridge contract tests, without network or live execution."""

import argparse
import json
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.skill_bridge import handle

if TYPE_CHECKING:
    from pathlib import Path


def test_doctor_cannot_claim_live_dispatch() -> None:
    output = handle(argparse.Namespace(command='doctor'))
    assert output['live_execution_qualified'] is False
    assert output['tool_sandbox_qualified'] is False


def test_plan_bridge_is_read_only(tmp_path: Path) -> None:
    path = tmp_path / 'plan.json'
    path.write_text(
        json.dumps({
            'id': 'task',
            'objective': 'Tests',
            'nodes': [{'id': 'one', 'objective': 'Write tests', 'owned_paths': ['tests/a.py']}],
        })
    )
    output = handle(argparse.Namespace(command='plan', file=path, session='sess'))
    assert output['ordered_nodes'] == ['one']
    assert output['dispatched'] is False
    assert list(tmp_path.iterdir()) == [path]


def test_plan_bridge_rejects_symlink(tmp_path: Path) -> None:
    source = tmp_path / 'source'
    source.write_text('{}')
    link = tmp_path / 'link'
    link.symlink_to(source)
    with pytest.raises(WorkflowError, match='symlinked'):
        handle(argparse.Namespace(command='plan', file=link, session='sess'))


def test_rejects_empty_session(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='session'):
        handle(argparse.Namespace(command='inspect', state=tmp_path, run_id='x', session=' '))
