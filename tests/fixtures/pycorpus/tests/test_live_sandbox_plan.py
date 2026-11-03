"""Command-construction tests; no sudo, subprocess, Hermes or network execution."""

from dataclasses import replace
from pathlib import Path

import pytest

from hermes_harness.sandbox.live_sandbox_plan import (
    SandboxInputs,
    SandboxPlanError,
    build_sandbox_plan,
)

EXPECTED_READONLY_MOUNTS = 3


@pytest.fixture
def inputs(tmp_path: Path) -> SandboxInputs:
    work = tmp_path / 'work'
    evidence = tmp_path / 'evidence'
    work.mkdir()
    evidence.mkdir()
    return SandboxInputs(
        role='worker',
        unit='hermes-job-test001',
        workspace=work,
        evidence=evidence,
        executable=Path('/usr/bin/env').resolve(strict=True),
        argv=('hello',),
    )


def test_worker_mount_is_writeable_but_evidence_read_only(inputs: SandboxInputs) -> None:
    args = build_sandbox_plan(inputs).systemd_argv
    index = args.index('--bind')
    assert args[index : index + 3] == ('--bind', str(inputs.workspace.resolve()), '/workspace')
    index = args.index(str(inputs.evidence.resolve()))
    assert args[index - 1 : index + 2] == ('--ro-bind', str(inputs.evidence.resolve()), '/evidence')
    assert '--unshare-net' in args
    assert '--unshare-pid' in args
    assert '--property=KillMode=control-group' in args
    assert '--uid=hermes' in args
    assert '/home/hermes/.hermes' not in args


def test_reviewer_has_no_writable_mount(inputs: SandboxInputs) -> None:
    args = build_sandbox_plan(replace(inputs, role='reviewer')).systemd_argv
    assert '--bind' not in args
    assert args.count('--ro-bind') == EXPECTED_READONLY_MOUNTS


@pytest.mark.parametrize('unit', ['evil', 'hermes-job-../../tmp', 'hermes-job-x;ls'])
def test_unsafe_unit_rejected(inputs: SandboxInputs, unit: str) -> None:
    with pytest.raises(SandboxPlanError):
        build_sandbox_plan(replace(inputs, unit=unit))


def test_symlink_mount_rejected(inputs: SandboxInputs, tmp_path: Path) -> None:
    link = tmp_path / 'link'
    link.symlink_to(inputs.workspace)
    with pytest.raises(SandboxPlanError):
        build_sandbox_plan(replace(inputs, workspace=link))


def test_overlapping_mounts_rejected(inputs: SandboxInputs) -> None:
    with pytest.raises(SandboxPlanError):
        build_sandbox_plan(replace(inputs, evidence=inputs.workspace))


def test_resource_override_rejected(inputs: SandboxInputs) -> None:
    with pytest.raises(SandboxPlanError):
        build_sandbox_plan(replace(inputs, memory_max='unlimited'))


def test_executable_from_workspace_rejected(inputs: SandboxInputs) -> None:
    executable = inputs.workspace / 'agent'
    executable.write_text('x')
    with pytest.raises(SandboxPlanError):
        build_sandbox_plan(replace(inputs, executable=executable))


def test_stop_is_specific_unit_only(inputs: SandboxInputs) -> None:
    plan = build_sandbox_plan(inputs)
    assert plan.stop_argv == ('/usr/bin/systemctl', 'stop', 'hermes-job-test001.service')
    assert 'systemd_argv' in plan.as_json()
