"""Durable task-to-service identity safety checks with real SQLite ledgers."""

from dataclasses import replace
from pathlib import Path  # noqa: TC003

import pytest

from hermes_harness.services.live_service_journal import ServiceJournal, ServiceState
from hermes_harness.supervised.live_execution_binding import (
    ExecutionBinding,
    ExecutionBindingError,
    ExecutionBindingStore,
)


@pytest.fixture
def context(tmp_path: Path) -> tuple[ExecutionBindingStore, ServiceJournal, ExecutionBinding]:
    store = ExecutionBindingStore(tmp_path / 'bindings.sqlite')
    journal = ServiceJournal(tmp_path / 'services.sqlite')
    binding = ExecutionBinding(
        task_id='task-1',
        role='worker',
        submission_id='sub-1',
        session_id='sess-1',
        unit='hermes-job-task-1-worker',
    )
    return store, journal, binding


def test_persist_and_reopen(
    context: tuple[ExecutionBindingStore, ServiceJournal, ExecutionBinding],
) -> None:
    store, journal, binding = context
    store.bind(binding, journal)
    reopened = ExecutionBindingStore(store.path)
    assert reopened.get(binding.task_id, 'worker') == binding
    with pytest.raises(ExecutionBindingError, match='Duplicate'):
        reopened.bind(binding, journal)


@pytest.mark.parametrize('field', ['role', 'submission_id', 'session_id', 'unit'])
def test_identity_cannot_be_reused(
    context: tuple[ExecutionBindingStore, ServiceJournal, ExecutionBinding],
    field: str,
) -> None:
    store, journal, binding = context
    store.bind(binding, journal)
    second = replace(
        binding,
        task_id='task-2',
        role='reviewer',
        submission_id='sub-2',
        session_id='sess-2',
        unit='hermes-job-task-2-reviewer',
    )
    if field == 'role':
        second = replace(second, task_id=binding.task_id)
    second = replace(second, **{field: getattr(binding, field)})
    with pytest.raises(ExecutionBindingError):
        store.bind(second, journal)


def test_running_requires_exact_record(
    context: tuple[ExecutionBindingStore, ServiceJournal, ExecutionBinding],
) -> None:
    store, journal, binding = context
    store.bind(binding, journal)
    with pytest.raises(ExecutionBindingError, match='not durably'):
        store.require_running(binding, journal)
    journal.reserve(unit=binding.unit, role='worker', plan_json='{}')
    with pytest.raises(ExecutionBindingError, match='not durably'):
        store.require_running(binding, journal)
    journal.transition(
        binding.unit,
        expected=ServiceState.LAUNCH_UNCERTAIN,
        target=ServiceState.RUNNING_OBSERVED,
    )
    store.require_running(binding, journal)
    with pytest.raises(ExecutionBindingError, match='mismatch'):
        store.require_running(replace(binding, session_id='tampered'), journal)


def test_symlink_store_rejected(tmp_path: Path) -> None:
    target = tmp_path / 'target'
    target.write_text('not sqlite')
    link = tmp_path / 'link'
    link.symlink_to(target)
    with pytest.raises(ExecutionBindingError, match='symlinks'):
        ExecutionBindingStore(link)
