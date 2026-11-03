"""Tests for durable local admission records."""

from typing import TYPE_CHECKING

import pytest

from hermes_harness.gateway.admission_store import (
    AdmissionStore,
    AdmissionStoreError,
    LocalState,
)
from hermes_harness.gateway.gateway_identity import Receipt, Submission

if TYPE_CHECKING:
    from pathlib import Path

FILE_MODE_MASK = 0o777
PRIVATE_FILE_MODE = 0o600


def make_store(tmp_path: Path) -> AdmissionStore:
    return AdmissionStore(tmp_path / 'private' / 'admissions.sqlite3')


def test_prepare_persists_before_network_io_and_uses_private_mode(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    record = store.prepare(Submission('session-a', 'submission-a'))

    assert record.local_state is LocalState.PREPARED
    assert record.admission_id is None
    file_mode = (tmp_path / 'private' / 'admissions.sqlite3').stat().st_mode & FILE_MODE_MASK
    assert file_mode == PRIVATE_FILE_MODE
    assert store.get('submission-a') == record


def test_duplicate_submission_does_not_overwrite_record(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    original = store.prepare(Submission('session-a', 'submission-a'))

    with pytest.raises(AdmissionStoreError, match='already exists'):
        store.prepare(Submission('session-b', 'submission-a'))

    assert store.get('submission-a') == original


def test_unknown_outcome_survives_reopen(tmp_path: Path) -> None:
    path = tmp_path / 'admissions.sqlite3'
    store = AdmissionStore(path)
    store.prepare(Submission('session-a', 'submission-a'))
    store.mark_outcome_unknown('submission-a')

    reopened = AdmissionStore(path)
    record = reopened.get('submission-a')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert record.admission_id is None


def test_receipt_records_distinct_admission_id(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    store.prepare(Submission('session-a', 'submission-a'))

    record = store.record_receipt('submission-a', Receipt('admission-a', 'queued', 1))

    assert record.submission_id == 'submission-a'
    assert record.admission_id == 'admission-a'
    assert record.local_state is LocalState.ADMITTED
    assert record.gateway_status == 'queued'
    assert record.execution_generation == 1


def test_receipt_cannot_change_admission_identity(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    store.prepare(Submission('session-a', 'submission-a'))
    store.record_receipt('submission-a', Receipt('admission-a', 'queued'))

    with pytest.raises(AdmissionStoreError, match='conflicts'):
        store.record_receipt('submission-a', Receipt('admission-b', 'running'))


def test_admission_id_is_unique_across_submissions(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    store.prepare(Submission('session-a', 'submission-a'))
    store.prepare(Submission('session-a', 'submission-b'))
    store.record_receipt('submission-a', Receipt('admission-a', 'queued'))

    with pytest.raises(AdmissionStoreError, match='already assigned'):
        store.record_receipt('submission-b', Receipt('admission-a', 'queued'))


def test_reconciliation_is_marked_separately(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    store.prepare(Submission('session-a', 'submission-a'))

    record = store.record_receipt(
        'submission-a', Receipt('admission-a', 'running'), reconciled=True
    )

    assert record.local_state is LocalState.RECEIPT_OBSERVED


def test_unknown_submission_and_bad_limit_are_rejected(tmp_path: Path) -> None:
    store = make_store(tmp_path)
    with pytest.raises(AdmissionStoreError, match='Unknown submission'):
        store.mark_outcome_unknown('missing')
    with pytest.raises(ValueError, match='limit'):
        store.list_records(limit=0)


def test_symlink_database_path_is_rejected(tmp_path: Path) -> None:
    target = tmp_path / 'target.sqlite3'
    target.touch()
    link = tmp_path / 'link.sqlite3'
    link.symlink_to(target)

    with pytest.raises(AdmissionStoreError, match='symbolic link'):
        AdmissionStore(link)
