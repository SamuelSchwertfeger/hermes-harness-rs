"""Check that the synthetic host probe never authorizes submission or mutation."""

import ast
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts' / 'probe_unknown_recovery.py'


def test_host_probe_has_no_submission_or_cancel_rpc() -> None:
    """Restrict the dedicated host probe to the existing recovery interface."""
    source = SCRIPT.read_text(encoding='utf-8')
    tree = ast.parse(source)
    attributes = {node.attr for node in ast.walk(tree) if isinstance(node, ast.Attribute)}
    assert 'submit' not in attributes
    assert 'cancel' not in attributes
    assert 'ensure_gateway_runtime' not in source
    assert 'prompt.submit' not in source
    assert 'prompt.cancel' not in source
    assert 'recover_with_existing_gateway' in source


def test_host_probe_requires_explicit_session_and_temporary_ledger() -> None:
    """Never default to a discovered session or a persistent database."""
    source = SCRIPT.read_text(encoding='utf-8')
    assert "parser.add_argument('--session-id', required=True)" in source
    assert "parser.add_argument('--profile-home', required=True)" in source
    assert 'TemporaryDirectory(' in source
    assert 'mark_outcome_unknown(submission_id)' in source
    assert 'LocalState.OUTCOME_UNKNOWN' in source
    assert 'ReconciliationStatus.STILL_UNKNOWN' in source


def test_host_probe_is_parseable() -> None:
    """The optional Hermes-dependent script remains valid Python syntax."""
    ast.parse(SCRIPT.read_text(encoding='utf-8'))
