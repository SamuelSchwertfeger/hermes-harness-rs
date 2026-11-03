import json
import subprocess
from typing import TYPE_CHECKING

import pytest

from hermes_harness.operator.live_host_probe import (
    HostProbeError,
    ProbeCommands,
    ProbeObservation,
    assess_observation,
    observe_unit,
    validate_pid_snapshot,
    write_probe_evidence,
)

"""Provider-free host probe contract tests."""


if TYPE_CHECKING:
    from pathlib import Path

UNIT = 'hermes-verify-12345678'


def test_rejects_arbitrary_units() -> None:
    for unit in ('hermes-job-prod', 'hermes-verify-1;rm', 'other', 'hermes-verify-../bad'):
        with pytest.raises(HostProbeError, match='verification namespace'):
            ProbeCommands(unit)


def test_property_allowlist() -> None:
    commands = ProbeCommands(UNIT)
    with pytest.raises(HostProbeError, match='Unrecognized'):
        commands.show('Environment')
    assert commands.stop() == ('/usr/bin/systemctl', 'stop', f'{UNIT}.service')


def test_default_transport_is_disabled() -> None:
    with pytest.raises(HostProbeError, match='disabled'):
        observe_unit(ProbeCommands(UNIT))


def test_readonly_exact_property_commands() -> None:
    calls: list[tuple[str, ...]] = []

    def runner(argv: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
        calls.append(argv)
        return subprocess.CompletedProcess(argv, 0, stdout='test\n', stderr='')

    observations = observe_unit(ProbeCommands(UNIT), runner)
    assert observations['User'] == 'test'
    assert all(call[:3] == ('/usr/bin/systemctl', 'show', '--property') for call in calls)
    assert len(calls) == len(observations)


def test_nonzero_runner_fails_closed() -> None:
    def failure(argv: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
        return subprocess.CompletedProcess(argv, 1, stdout='', stderr='denied')

    with pytest.raises(HostProbeError, match='failed'):
        observe_unit(ProbeCommands(UNIT), failure)


def test_exact_effective_controls() -> None:
    values = {
        'ActiveState': 'active',
        'MainPID': '42',
        'User': 'hermes',
        'MemoryMax': '536870912',
        'CPUQuotaPerSecUSec': '500ms',
        'TasksMax': '32',
        'KillMode': 'control-group',
        'ControlGroup': '/system.slice/hermes-verify-12345678.service',
    }
    assert assess_observation(values) == ()
    values['MemoryMax'] = 'infinity'
    assert 'MemoryMax' in assess_observation(values)
    values['MainPID'] = '0'
    assert 'MainPID' in assess_observation(values)


def test_process_snapshot_refuses_survivors() -> None:
    validate_pid_snapshot((100, 101), ())
    with pytest.raises(HostProbeError, match='remained'):
        validate_pid_snapshot((100, 101), (101,))
    with pytest.raises(HostProbeError, match='Invalid or empty'):
        validate_pid_snapshot((), ())


def test_evidence_is_unapproved_and_exclusive(tmp_path: Path) -> None:
    path = tmp_path / 'evidence.json'
    observation = ProbeObservation(UNIT, {'User': 'hermes'}, (100,), (), 'inactive')
    write_probe_evidence(path, observation)
    payload = json.loads(path.read_text())
    assert payload['qualified'] is False
    with pytest.raises(HostProbeError, match='already exists'):
        write_probe_evidence(path, observation)
