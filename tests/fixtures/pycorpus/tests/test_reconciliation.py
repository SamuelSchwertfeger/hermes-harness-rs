"""Tests for fail-closed reconciliation of uncertain submissions."""

from typing import TYPE_CHECKING

import pytest

from hermes_harness.core.reconciliation import (
    LookupDisposition,
    LookupResult,
    ReconciliationError,
    ReconciliationStatus,
    reconcile_unknown,
)
from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_identity import Receipt, Submission

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.gateway.admission_store import AdmissionRecord


class Resolver:
    """Deterministic fake for the injected verified lookup contract."""

    def __init__(self, result: LookupResult) -> None:
        self.result = result
        self.calls = 0

    def resolve(self, _record: AdmissionRecord) -> LookupResult:
        self.calls += 1
        assert _record.submission_id == 'sub-1'
        return self.result


def uncertain_store(tmp_path: Path) -> AdmissionStore:
    store = AdmissionStore(tmp_path / 'private' / 'admissions.sqlite3')
    store.prepare(Submission(session_id='session-1', submission_id='sub-1'))
    store.mark_outcome_unknown('sub-1')
    return store


def test_found_receipt_is_persisted_as_reconciled(tmp_path: Path) -> None:
    store = uncertain_store(tmp_path)
    resolver = Resolver(LookupResult(LookupDisposition.FOUND, Receipt('adm-1', 'queued', 2)))

    result = reconcile_unknown(store, 'sub-1', resolver=resolver)

    assert result is ReconciliationStatus.RECEIPT_RECORDED
    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.RECEIPT_OBSERVED
    assert record.admission_id == 'adm-1'
    assert resolver.calls == 1


@pytest.mark.parametrize(
    'disposition', [LookupDisposition.NOT_FOUND, LookupDisposition.INDETERMINATE]
)
def test_missing_or_ambiguous_lookup_keeps_unknown(
    tmp_path: Path, disposition: LookupDisposition
) -> None:
    store = uncertain_store(tmp_path)
    resolver = Resolver(LookupResult(disposition))

    result = reconcile_unknown(store, 'sub-1', resolver=resolver)

    assert result is ReconciliationStatus.STILL_UNKNOWN
    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
    assert record.admission_id is None
    assert resolver.calls == 1


def test_known_admission_identity_cannot_change(tmp_path: Path) -> None:
    store = uncertain_store(tmp_path)
    store.record_receipt('sub-1', Receipt('adm-original', 'queued'))
    store.mark_outcome_unknown('sub-1')
    resolver = Resolver(LookupResult(LookupDisposition.FOUND, Receipt('adm-other', 'queued')))

    with pytest.raises(ReconciliationError, match='conflicts'):
        reconcile_unknown(store, 'sub-1', resolver=resolver)

    record = store.get('sub-1')
    assert record is not None
    assert record.admission_id == 'adm-original'


def test_only_unknown_records_are_eligible(tmp_path: Path) -> None:
    store = AdmissionStore(tmp_path / 'admissions.sqlite3')
    store.prepare(Submission(session_id='session-1', submission_id='sub-1'))
    resolver = Resolver(LookupResult(LookupDisposition.INDETERMINATE))

    result = reconcile_unknown(store, 'sub-1', resolver=resolver)

    assert result is ReconciliationStatus.NOT_ELIGIBLE
    assert resolver.calls == 0


def test_unknown_submission_is_not_eligible(tmp_path: Path) -> None:
    store = uncertain_store(tmp_path)
    resolver = Resolver(LookupResult(LookupDisposition.INDETERMINATE))

    result = reconcile_unknown(store, 'missing', resolver=resolver)

    assert result is ReconciliationStatus.NOT_ELIGIBLE
    assert resolver.calls == 0


def test_invalid_lookup_result_shape_is_rejected() -> None:
    with pytest.raises(ValueError, match='receipt is required'):
        LookupResult(LookupDisposition.FOUND)

    with pytest.raises(ValueError, match='receipt is required'):
        LookupResult(LookupDisposition.NOT_FOUND, Receipt('adm-1', 'queued'))


def test_resolver_exception_preserves_unknown_record(tmp_path: Path) -> None:
    store = uncertain_store(tmp_path)

    class FailingResolver:
        def resolve(self, _record: AdmissionRecord) -> LookupResult:
            msg = 'simulated disconnect'
            raise OSError(msg)

    with pytest.raises(OSError, match='simulated disconnect'):
        reconcile_unknown(store, 'sub-1', resolver=FailingResolver())

    record = store.get('sub-1')
    assert record is not None
    assert record.local_state is LocalState.OUTCOME_UNKNOWN
