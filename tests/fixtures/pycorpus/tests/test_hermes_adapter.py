"""No real gateway, Ollama server or Codex client is started in these tests."""

import json
from pathlib import Path  # noqa: TC003 - CI Python 3.13 runtime annotations

import pytest

from hermes_harness.gateway.hermes_adapter import (
    HermesSafetyError,
    TaskKind,
    TaskRequest,
    execute,
    prepare,
)


def _request(tmp_path: Path, kind: TaskKind = TaskKind.SCOUT) -> TaskRequest:
    return TaskRequest(ticket_id='m2-001', kind=kind, prompt='Review this test', workspace=tmp_path)


def test_granite_route_is_disabled_for_execution(tmp_path: Path) -> None:
    prepared = prepare(_request(tmp_path))
    assert (prepared.provider, prepared.model) == ('ollama', 'granite4:3b')
    assert not prepared.execution_enabled
    with pytest.raises(HermesSafetyError, match='execution disabled'):
        execute(prepared)


def test_codex_plan_route(tmp_path: Path) -> None:
    prepared = prepare(_request(tmp_path, TaskKind.PLAN))
    assert (prepared.provider, prepared.model) == ('openai-codex', 'gpt-6.1-sol')


def test_scope_and_cyber_disabled(tmp_path: Path) -> None:
    with pytest.raises(PermissionError, match='authorized scope'):
        prepare(_request(tmp_path, TaskKind.CYBER))
    scoped = TaskRequest(
        ticket_id='m2-001',
        kind=TaskKind.CYBER,
        prompt='Offline defensive review',
        workspace=tmp_path,
        authorized_scope='local-test-fixtures',
    )
    with pytest.raises(HermesSafetyError, match='Cyber adapter disabled'):
        prepare(scoped)


def test_budget_and_workspace_validation(tmp_path: Path) -> None:
    invalid = TaskRequest(
        ticket_id='m2-001',
        kind=TaskKind.SCOUT,
        prompt='Review',
        workspace=tmp_path,
        max_turns=0,
    )
    with pytest.raises(ValueError, match='budgets'):
        prepare(invalid)


def test_private_manifest_no_overwrite(tmp_path: Path) -> None:
    prepared = prepare(_request(tmp_path))
    destination = tmp_path / 'runs' / 'request.json'
    prepared.save(destination)
    expected_permissions = 0o600
    assert destination.stat().st_mode & 0o777 == expected_permissions
    assert json.loads(destination.read_text())['execution_enabled'] is False
    with pytest.raises(FileExistsError, match='overwrite'):
        prepared.save(destination)
