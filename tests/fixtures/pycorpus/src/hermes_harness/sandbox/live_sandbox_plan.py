"""Render bounded systemd/Bubblewrap plans; never execute privileged commands.

These plans are operator-reviewable launch specifications, not runtime qualification
or permission to dispatch live Hermes gateway requests.
"""

import json
import re
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Literal

Role = Literal['worker', 'reviewer']
_UNIT = re.compile(r'[a-zA-Z0-9][a-zA-Z0-9_.-]{0,63}\Z')


class SandboxPlanError(ValueError):
    """An unsafe or incomplete sandbox plan was rejected."""


@dataclass(frozen=True, slots=True, kw_only=True)
class SandboxInputs:
    """Operator-chosen paths, fixed resource policy and role identity."""

    role: Role
    unit: str
    workspace: Path
    evidence: Path
    executable: Path
    argv: tuple[str, ...]
    memory_max: str = '512M'
    cpu_quota: str = '50%'
    tasks_max: int = 32


@dataclass(frozen=True, slots=True)
class SandboxPlan:
    """Immutable structured plan, suitable for inspecting before manual execution."""

    role: Role
    unit: str
    systemd_argv: tuple[str, ...]
    stop_argv: tuple[str, ...]

    def as_json(self) -> str:
        """Serialize argv without shell interpretation or execution."""
        return json.dumps(
            {
                'role': self.role,
                'unit': self.unit,
                'systemd_argv': self.systemd_argv,
                'stop_argv': self.stop_argv,
            },
            indent=2,
        )


def _directory(path: Path) -> Path:
    if path.is_symlink() or not path.is_dir():
        msg = 'Mount source must be a real existing directory'
        raise SandboxPlanError(msg)
    resolved = path.resolve(strict=True)
    if resolved == Path('/') or resolved == Path('/home/hermes'):
        msg = 'Host root and Hermes home must never be mounted'
        raise SandboxPlanError(msg)
    return resolved


def _validate_sandbox_inputs(inputs: SandboxInputs) -> tuple[Path, Path, Path]:
    """Validate all operator-chosen data before constructing a launch command."""
    if inputs.role not in ('worker', 'reviewer'):
        msg = 'Unknown role'
        raise SandboxPlanError(msg)
    if not _UNIT.fullmatch(inputs.unit) or not inputs.unit.startswith('hermes-job-'):
        msg = 'Unit must use the hermes-job- namespace'
        raise SandboxPlanError(msg)
    workspace = _directory(inputs.workspace)
    evidence = _directory(inputs.evidence)
    if workspace == evidence or workspace in evidence.parents or evidence in workspace.parents:
        msg = 'Workspace and evidence mounts must be disjoint'
        raise SandboxPlanError(msg)
    if inputs.executable.is_symlink() or not inputs.executable.is_file():
        msg = 'Executable must be a real existing file'
        raise SandboxPlanError(msg)
    executable = inputs.executable.resolve(strict=True)
    if not executable.is_relative_to(Path('/usr')):
        msg = 'Only system-installed executables under /usr are supported'
        raise SandboxPlanError(msg)
    if not inputs.argv or any(not value or '\x00' in value for value in inputs.argv):
        msg = 'Explicit nonempty argv is required'
        raise SandboxPlanError(msg)
    if (inputs.memory_max, inputs.cpu_quota, inputs.tasks_max) != ('512M', '50%', 32):
        msg = 'Resource policy is fixed until host-qualified alternatives exist'
        raise SandboxPlanError(msg)

    return workspace, evidence, executable


def build_sandbox_plan(inputs: SandboxInputs) -> SandboxPlan:
    """Build an isolated job plan; refuse permissive or ambiguous configuration."""
    workspace, evidence, executable = _validate_sandbox_inputs(inputs)

    role_mount = '--bind' if inputs.role == 'worker' else '--ro-bind'
    bwrap = (
        '/usr/bin/bwrap',
        '--unshare-user',
        '--unshare-pid',
        '--unshare-net',
        '--die-with-parent',
        '--new-session',
        '--ro-bind',
        '/usr',
        '/usr',
        '--proc',
        '/proc',
        '--dev',
        '/dev',
        '--tmpfs',
        PurePosixPath('/', 'tmp').as_posix(),
        '--dir',
        '/workspace',
        '--dir',
        '/evidence',
        role_mount,
        str(workspace),
        '/workspace',
        '--ro-bind',
        str(evidence),
        '/evidence',
        '--chdir',
        '/workspace',
        str(executable),
        *inputs.argv,
    )
    command = (
        '/usr/bin/systemd-run',
        f'--unit={inputs.unit}',
        '--uid=hermes',
        '--property=MemoryMax=512M',
        '--property=CPUQuota=50%',
        '--property=TasksMax=32',
        '--property=KillMode=control-group',
        '--property=TimeoutStopSec=5s',
        '--property=NoNewPrivileges=yes',
        '--property=PrivateDevices=yes',
        '--property=ProtectSystem=strict',
        *bwrap,
    )
    return SandboxPlan(
        inputs.role,
        inputs.unit,
        command,
        ('/usr/bin/systemctl', 'stop', f'{inputs.unit}.service'),
    )


# Temporary exports for legacy module import compatibility.
__all__ = [
    '_UNIT',
    'Literal',
    'Path',
    'PurePosixPath',
    'Role',
    'SandboxInputs',
    'SandboxPlan',
    'SandboxPlanError',
    '_directory',
    '_validate_sandbox_inputs',
    'build_sandbox_plan',
    'dataclass',
    'json',
    're',
]
