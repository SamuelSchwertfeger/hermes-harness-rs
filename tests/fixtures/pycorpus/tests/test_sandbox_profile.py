"""Sandbox profile is inspectable but never executed by these tests."""

from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.multiagent.sandbox_profile import SandboxSpec, bubblewrap_argv

if TYPE_CHECKING:
    from pathlib import Path


def test_bubblewrap_profile_denies_network_and_root_writes(tmp_path: Path) -> None:
    executable = tmp_path / 'pytest'
    executable.write_text('# placeholder')
    command = bubblewrap_argv(SandboxSpec(tmp_path, executable, ('-q',)))
    assert '--unshare-all' in command
    assert command[command.index('--ro-bind') + 1] == '/'
    assert '--tmpfs' in command
    assert '--' in command
    assert command[-2:] == (str(executable), '-q')


def test_sandbox_refuses_bad_timeout(tmp_path: Path) -> None:
    executable = tmp_path / 'python'
    executable.write_text('')
    with pytest.raises(WorkflowError, match='Invalid bubblewrap'):
        bubblewrap_argv(SandboxSpec(tmp_path, executable, ('-m', 'pytest'), 0))


def test_sandbox_refuses_command_injection(tmp_path: Path) -> None:
    executable = tmp_path / 'python'
    executable.write_text('')
    with pytest.raises(WorkflowError, match='argument'):
        bubblewrap_argv(SandboxSpec(tmp_path, executable, ('a\x00b',)))
