"""Linux bubblewrap command construction for independent verifier qualification.

This is a proposed OS isolation profile, not an attested sandbox. It is
deliberately *not* wired to automatic execution or trusted-code flags.
Operator must qualify user namespaces, filesystem mounts, /proc, seccomp,
resource limits and device exposure on the actual Debian host first.
"""

from dataclasses import dataclass
from pathlib import Path

from hermes_harness.multiagent.runtime import WorkflowError

MAX_SANDBOX_SECONDS = 180

MAX_SANDBOX_ARGS = 16

MAX_SANDBOX_ARG_LENGTH = 512

SANDBOX_TMP = '/tmp'  # noqa: S108 - private bubblewrap tmpfs, not host tempfile


@dataclass(frozen=True, slots=True)
class SandboxSpec:
    worktree: Path
    executable: Path
    args: tuple[str, ...]
    max_seconds: int = 60


def bubblewrap_argv(spec: SandboxSpec) -> tuple[str, ...]:
    """Produce fixed network-disabled, read-only-root argv without executing it."""
    root = spec.worktree
    executable = spec.executable
    root_ok = root.is_absolute() and not root.is_symlink() and root.is_dir()
    executable_ok = (
        executable.is_absolute() and not executable.is_symlink() and executable.is_file()
    )
    duration_ok = 1 <= spec.max_seconds <= MAX_SANDBOX_SECONDS
    if not (root_ok and executable_ok and duration_ok):
        msg = 'Invalid bubblewrap worktree, executable or timeout'
        raise WorkflowError(msg)
    if root.resolve() == Path('/') or root.resolve() == Path('/home'):
        msg = 'Refusing overly broad sandbox worktree'
        raise WorkflowError(msg)
    if not spec.args or len(spec.args) > MAX_SANDBOX_ARGS:
        msg = 'Invalid sandbox command cardinality'
        raise WorkflowError(msg)
    if any(not item or '\x00' in item or len(item) > MAX_SANDBOX_ARG_LENGTH for item in spec.args):
        msg = 'Invalid sandbox command argument'
        raise WorkflowError(msg)
    # The code tree is mounted read-only and the process receives a private
    # /tmp. This is an isolation *proposal*, not proof of safe execution.
    return (
        'bwrap',
        '--die-with-parent',
        '--new-session',
        '--unshare-all',
        '--ro-bind',
        '/',
        '/',
        '--tmpfs',
        SANDBOX_TMP,
        '--proc',
        '/proc',
        '--dev',
        '/dev',
        '--chdir',
        str(root),
        '--setenv',
        'HOME',
        SANDBOX_TMP,
        '--setenv',
        'PYTHONDONTWRITEBYTECODE',
        '1',
        '--setenv',
        'PYTEST_DISABLE_PLUGIN_AUTOLOAD',
        '1',
        '--setenv',
        'UV_OFFLINE',
        '1',
        '--',
        str(executable),
        *spec.args,
    )
