"""Offline gates for the composed disposable-session host probe."""

import ast
import json
import subprocess
import sys
from pathlib import Path

import pytest

CLI_USAGE_ERROR = 2
SCRIPT = Path(__file__).resolve().parents[1] / 'scripts' / 'probe_disposable_admission.py'


def arguments(tmp_path: Path) -> list[str]:
    """Provide valid parser inputs but do not opt into execution."""
    return [
        '--profile-home',
        '/nonexistent/hermes-profile',
        '--creation-record',
        str(tmp_path / 'creation.json'),
        '--ledger',
        str(tmp_path / 'ledger.sqlite'),
        '--create-request-id',
        'create-test',
        '--submission-id',
        'submit-test',
    ]


def invoke(args: list[str]) -> subprocess.CompletedProcess[str]:
    """Exercise the real CLI in a fresh interpreter without dynamic imports."""
    return subprocess.run(  # noqa: S603 — fixed interpreter and repository-owned script.
        [sys.executable, str(SCRIPT), *args],
        capture_output=True,
        text=True,
        check=False,
        timeout=15,
    )


def test_preflight_is_offline_and_creates_no_records(tmp_path: Path) -> None:
    result = invoke(arguments(tmp_path))
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {
        'result': 'preflight_only',
        'gateway_contacted': False,
        'submission_enabled': False,
    }
    assert list(tmp_path.iterdir()) == []


@pytest.mark.parametrize('confirmation', ['incorrect', ''])
def test_bad_confirmation_rejected_before_gateway(tmp_path: Path, confirmation: str) -> None:
    result = invoke([
        *arguments(tmp_path),
        '--execute',
        '--confirm-disposable',
        '--confirm-create-request-id',
        confirmation,
    ])
    assert result.returncode == CLI_USAGE_ERROR
    assert json.loads(result.stdout) == {
        'result': 'unavailable',
        'error_type': 'ValueError',
    }
    assert list(tmp_path.iterdir()) == []


def test_missing_disposable_ack_rejected(tmp_path: Path) -> None:
    result = invoke([
        *arguments(tmp_path),
        '--execute',
        '--confirm-create-request-id',
        'create-test',
    ])
    assert result.returncode == CLI_USAGE_ERROR
    assert json.loads(result.stdout)['error_type'] == 'ValueError'
    assert list(tmp_path.iterdir()) == []


@pytest.mark.parametrize('occupied', ['creation.json', 'ledger.sqlite'])
def test_reused_durable_paths_rejected(tmp_path: Path, occupied: str) -> None:
    (tmp_path / occupied).touch()
    result = invoke([
        *arguments(tmp_path),
        '--execute',
        '--confirm-disposable',
        '--confirm-create-request-id',
        'create-test',
    ])
    assert result.returncode == CLI_USAGE_ERROR
    assert json.loads(result.stdout)['error_type'] == 'ValueError'


def test_no_gateway_autostart_or_hidden_submission() -> None:
    source = SCRIPT.read_text(encoding='utf-8')
    tree = ast.parse(source)
    assert 'ensure_gateway_runtime' not in source
    assert 'session.resume' not in source
    assert 'prompt.submit' not in source
    assert 'create_and_submit_once(rpc, store, request)' in source
    assert any(isinstance(node, ast.AsyncWith) for node in ast.walk(tree))
