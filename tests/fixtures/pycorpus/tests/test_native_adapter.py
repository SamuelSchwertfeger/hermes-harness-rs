"""Hermes-facing API regression tests for offline-only operation."""

import json
from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.local_inference import LocalEndpoint
from hermes_harness.multiagent.runtime import Node, Plan, WorkflowError
from hermes_harness.native.workflows import (
    NativeContext,
    admit_plan_json,
    agents_assemble_offline,
    game_plan,
    propose_local_edits,
)

if TYPE_CHECKING:
    from pathlib import Path


def _context(tmp_path: Path) -> NativeContext:
    repo = tmp_path / 'repo'
    state = tmp_path / 'state'
    repo.mkdir()
    state.mkdir()
    return NativeContext('hermes-session', repo, state)


def _plan() -> Plan:
    return Plan('ticket', 'Task', (Node('worker', 'Write test', ('tests/new.py',)),))


def test_native_plan_echoes_session(tmp_path: Path) -> None:
    result = game_plan(_context(tmp_path), _plan())
    assert result['session_id'] == 'hermes-session'
    assert result['ordered_nodes'] == ['worker']
    assert result['mode'] == 'validation-only'


def test_admit_model_plan_before_dispatch(tmp_path: Path) -> None:
    payload = {
        'id': 'ticket',
        'objective': 'Task',
        'nodes': [{'id': 'worker', 'objective': 'Write test', 'owned_paths': ['tests/new.py']}],
    }
    result = admit_plan_json(_context(tmp_path), json.dumps(payload))
    assert result['plan_id'] == 'ticket'
    assert not (tmp_path / 'state/ticket').exists()


def test_offline_dispatch_requires_explicit_authorization(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='Explicit offline authorization'):
        agents_assemble_offline(_context(tmp_path), _plan())
    assert not (tmp_path / 'state/ticket').exists()


def test_native_cyber_dispatch_refused(tmp_path: Path) -> None:
    plan = Plan(
        'ticket',
        'Task',
        (Node('worker', 'Review', ('tests/new.py',), role='cyber-warrior'),),
    )
    with pytest.raises(WorkflowError, match='grant transport'):
        agents_assemble_offline(_context(tmp_path), plan, authorized=True)
    assert not (tmp_path / 'state/ticket').exists()


def test_invalid_session_refused(tmp_path: Path) -> None:
    context = _context(tmp_path)
    with pytest.raises(WorkflowError, match='Invalid originating'):
        game_plan(NativeContext(' ', context.repository, context.state_root), _plan())


def test_native_local_proposal_requires_explicit_grant(tmp_path: Path) -> None:
    class NoTransport:
        def complete(self, _endpoint: LocalEndpoint, _payload: bytes) -> bytes:
            msg = 'No model calls permitted without authorization'
            raise AssertionError(msg)

    endpoint = LocalEndpoint('http://127.0.0.1:8080/v1/chat/completions', 'test')
    with pytest.raises(WorkflowError, match='Explicit local inference'):
        propose_local_edits(_context(tmp_path), endpoint, _plan().nodes[0], NoTransport())
