"""Read-only cross-ledger inspector integration with actual disposable SQLite files."""

import json
import sqlite3
from typing import TYPE_CHECKING

import pytest

from hermes_harness.operator.live_operator_report import (
    LedgerPaths,
    LiveReportError,
    inspect_task,
)

if TYPE_CHECKING:
    from pathlib import Path


def _ledgers(tmp_path: Path) -> LedgerPaths:
    paths = LedgerPaths(
        tmp_path / 'bindings.sqlite', tmp_path / 'services.sqlite', tmp_path / 'admissions.sqlite'
    )
    with sqlite3.connect(paths.bindings) as db:
        db.execute(
            'CREATE TABLE bindings ('
            'task_id TEXT, role TEXT, submission_id TEXT, session_id TEXT, unit TEXT)'
        )
        db.execute(
            "INSERT INTO bindings VALUES ('task-1','worker','sub-worker',"
            "'session-worker','hermes-job-worker')"
        )
        db.execute(
            "INSERT INTO bindings VALUES ('task-1','reviewer','sub-reviewer',"
            "'session-reviewer','hermes-job-reviewer')"
        )
    with sqlite3.connect(paths.services) as db:
        db.execute('CREATE TABLE services (unit TEXT, role TEXT, state TEXT)')
        db.execute("INSERT INTO services VALUES ('hermes-job-worker','worker','running_observed')")
        db.execute(
            "INSERT INTO services VALUES ('hermes-job-reviewer','reviewer','stop_uncertain')"
        )
    with sqlite3.connect(paths.admissions) as db:
        db.execute(
            'CREATE TABLE admissions ('
            'submission_id TEXT, session_id TEXT, admission_id TEXT, local_state TEXT)'
        )
        db.execute(
            "INSERT INTO admissions VALUES ('sub-worker','session-worker',"
            "'a-worker','receipt_observed')"
        )
        db.execute(
            "INSERT INTO admissions VALUES ('sub-reviewer','session-reviewer',"
            "NULL,'outcome_unknown')"
        )
    return paths


def test_joined_report_remains_non_authoritative(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    report = inspect_task(paths, 'task-1')
    worker, reviewer = report.roles
    assert worker.admission_id == 'a-worker'
    assert worker.concerns == ('live_process_not_verified',)
    assert 'service_not_recorded_running' in reviewer.concerns
    assert 'submission_uncertain_or_pending' in reviewer.concerns
    assert report.safe_to_dispatch is False
    assert report.safe_to_retry is False
    assert report.approved is False
    assert json.loads(report.as_json())['task_id'] == 'task-1'


def test_inspection_never_changes_database(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    before = tuple(path.read_bytes() for path in (paths.bindings, paths.services, paths.admissions))
    inspect_task(paths, 'task-1')
    inspect_task(paths, 'task-1')
    after = tuple(path.read_bytes() for path in (paths.bindings, paths.services, paths.admissions))
    assert before == after


def test_unknown_task_does_not_infer_previous_identity(tmp_path: Path) -> None:
    report = inspect_task(_ledgers(tmp_path), 'other-task')
    assert all('binding_missing' in role.concerns for role in report.roles)
    assert all(role.submission_id is None for role in report.roles)


def test_mismatched_session_detected(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    with sqlite3.connect(paths.admissions) as db:
        db.execute("UPDATE admissions SET session_id='tampered' WHERE submission_id='sub-worker'")
    report = inspect_task(paths, 'task-1')
    assert 'session_mismatch' in report.roles[0].concerns


def test_missing_service_detected(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    with sqlite3.connect(paths.services) as db:
        db.execute("DELETE FROM services WHERE unit='hermes-job-worker'")
    assert 'service_missing' in inspect_task(paths, 'task-1').roles[0].concerns


def test_service_role_tamper_detected(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    with sqlite3.connect(paths.services) as db:
        db.execute("UPDATE services SET role='reviewer' WHERE unit='hermes-job-worker'")
    assert 'service_role_mismatch' in inspect_task(paths, 'task-1').roles[0].concerns


def test_missing_database_refused_without_creation(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    paths.admissions.unlink()
    with pytest.raises(LiveReportError, match='existing regular'):
        inspect_task(paths, 'task-1')
    assert not paths.admissions.exists()


def test_symlinked_database_refused(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    link = tmp_path / 'linked.sqlite'
    link.symlink_to(paths.bindings)
    with pytest.raises(LiveReportError, match='non-symlink'):
        inspect_task(LedgerPaths(link, paths.services, paths.admissions), 'task-1')


def test_symlinked_parent_refused(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    linked_dir = tmp_path / 'linked-dir'
    linked_dir.symlink_to(tmp_path, target_is_directory=True)
    with pytest.raises(LiveReportError, match='parent'):
        inspect_task(
            LedgerPaths(linked_dir / 'bindings.sqlite', paths.services, paths.admissions),
            'task-1',
        )


def test_invalid_schema_refused(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    paths.bindings.write_text('not sqlite')
    with pytest.raises(LiveReportError, match='schema'):
        inspect_task(paths, 'task-1')


def test_duplicate_role_refused(tmp_path: Path) -> None:
    paths = _ledgers(tmp_path)
    with sqlite3.connect(paths.bindings) as db:
        db.execute(
            "INSERT INTO bindings VALUES ('task-1','worker','sub-extra',"
            "'session-extra','hermes-job-extra')"
        )
    with pytest.raises(LiveReportError, match='task-role'):
        inspect_task(paths, 'task-1')


def test_blank_task_refused(tmp_path: Path) -> None:
    with pytest.raises(LiveReportError, match='Nonblank'):
        inspect_task(_ledgers(tmp_path), ' ')
