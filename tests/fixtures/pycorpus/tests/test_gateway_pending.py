"""Offline pending admission identity tests; never connect to Hermes."""

from hermes_harness.gateway.gateway_pending import PendingDisposition, inspect_pending
from hermes_harness.gateway.gateway_readonly import SessionSnapshot


def _snapshot(pending: object, session_id: str = 'session-1') -> SessionSnapshot:
    return SessionSnapshot(session_id, {'pending': pending})


def test_unique_pending_match() -> None:
    snapshot = _snapshot([
        {'input_id': 'other', 'admission_id': 'adm-2'},
        {'input_id': 'sub-1', 'admission_id': 'adm-1'},
    ])
    result = inspect_pending(snapshot, 'session-1', 'sub-1')
    assert result.disposition is PendingDisposition.FOUND
    assert result.admission_id == 'adm-1'


def test_missing_pending_is_unknown() -> None:
    result = inspect_pending(_snapshot([]), 'session-1', 'sub-1')
    assert result.disposition is PendingDisposition.INDETERMINATE


def test_duplicate_match_is_unknown() -> None:
    snapshot = _snapshot([
        {'input_id': 'sub-1', 'admission_id': 'a'},
        {'input_id': 'sub-1', 'admission_id': 'b'},
    ])
    result = inspect_pending(snapshot, 'session-1', 'sub-1')
    assert result.disposition is PendingDisposition.INDETERMINATE


def test_invalid_identity_is_unknown() -> None:
    assert (
        inspect_pending(
            _snapshot([{'input_id': 'sub-1', 'admission_id': ''}]), 'session-1', 'sub-1'
        ).disposition
        is PendingDisposition.INDETERMINATE
    )


def test_wrong_session_is_unknown() -> None:
    snapshot = _snapshot([{'input_id': 'sub-1', 'admission_id': 'a'}])
    assert (
        inspect_pending(snapshot, 'session-2', 'sub-1').disposition
        is PendingDisposition.INDETERMINATE
    )


def test_malformed_pending_is_unknown() -> None:
    assert (
        inspect_pending(_snapshot('not-a-list'), 'session-1', 'sub-1').disposition
        is PendingDisposition.INDETERMINATE
    )
    result = inspect_pending(_snapshot([42]), 'session-1', 'sub-1')
    assert result.disposition is PendingDisposition.INDETERMINATE
