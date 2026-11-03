"""Check that the local communication probe cannot become provider execution."""

from typing import TYPE_CHECKING

from hermes_harness.gateway.gateway_socket_qualification import sandbox_command

if TYPE_CHECKING:
    from pathlib import Path


def test_fixed_bwrap_profile(tmp_path: Path) -> None:
    argv = sandbox_command(tmp_path)
    assert '--unshare-net' in argv
    assert '--unshare-pid' in argv
    assert '--die-with-parent' in argv
    assert '--ro-bind' in argv
    assert '/gateway' in argv
    assert '/home/hermes' not in argv
    assert 'sudo' not in argv


def test_command_has_no_provider_or_hermes_agent_invocation(tmp_path: Path) -> None:
    argv = sandbox_command(tmp_path)
    assert argv[-2] == '-c'
    assert argv[-3] == '/usr/bin/python3'
    assert not any('api_key' in part.lower() for part in argv)
