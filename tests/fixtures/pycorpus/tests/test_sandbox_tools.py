"""Negative and positive contract tests for sandboxed read-only agent tools."""

from typing import TYPE_CHECKING
from unittest.mock import patch

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.multiagent.sandbox_tools import (
    ToolResult,
    record_sandbox_tool,
    run_sandbox_tool,
    sandbox_tool_argv,
)

if TYPE_CHECKING:
    from pathlib import Path


def test_no_shell_network_home_or_write_mount(tmp_path: Path) -> None:
    with patch('pathlib.Path.is_file', return_value=True):
        args = sandbox_tool_argv(tmp_path, 'list-files')
    assert '--unshare-all' in args
    assert '--clearenv' in args
    assert 'sh' not in args
    assert ('--ro-bind', str(tmp_path), '/work') in tuple(
        zip(args, args[1:], args[2:], strict=False)
    )
    assert '--bind' not in args


def test_unknown_tool_and_invalid_pattern_fail_closed(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='Unknown'):
        sandbox_tool_argv(tmp_path, 'bash')
    with pytest.raises(WorkflowError, match='pattern'):
        sandbox_tool_argv(tmp_path, 'search-fixed', pattern='\x00')
    with pytest.raises(WorkflowError, match='pattern'):
        sandbox_tool_argv(tmp_path, 'search-fixed', pattern='\n')


def test_authorization_precedes_bwrap_lookup(tmp_path: Path) -> None:
    with pytest.raises(WorkflowError, match='authorization'):
        run_sandbox_tool(tmp_path, 'list-files')
    with (
        patch('shutil.which', return_value=None),
        pytest.raises(WorkflowError, match='Bubblewrap unavailable'),
    ):
        run_sandbox_tool(tmp_path, 'list-files', authorized=True)


def test_receipt_write_once(tmp_path: Path) -> None:
    evidence = tmp_path / 'tool.json'
    result = ToolResult(
        'list-files', 0, timed_out=False, stdout='file.py', stderr='', output_sha256='abc'
    )
    digest = record_sandbox_tool(result, evidence)
    assert len(digest) == 64  # noqa: PLR2004 - SHA-256 hex length
    assert evidence.stat().st_mode & 0o777 == 0o600  # noqa: PLR2004 - private mode
    with pytest.raises(WorkflowError, match='new path'):
        record_sandbox_tool(result, evidence)
