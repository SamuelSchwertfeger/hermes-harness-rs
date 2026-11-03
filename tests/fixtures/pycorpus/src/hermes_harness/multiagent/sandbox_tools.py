"""Explicitly authorized, read-only Bubblewrap tool calls for local agents.

This is a narrow primitive, not a general shell and not a live orchestration loop.
No executable, shell string, or filesystem path is selected by the model.
"""

import hashlib
import json
import os
import resource
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path

from hermes_harness.multiagent.runtime import WorkflowError

MAX_OUTPUT = 16384
MAX_SECONDS = 30
MAX_PATTERN = 256


@dataclass(frozen=True, slots=True)
class ToolResult:
    tool: str
    exit_code: int | None
    timed_out: bool
    stdout: str
    stderr: str
    output_sha256: str
    sandbox: str = 'bubblewrap-no-network-readonly-worktree'
    host_execution: bool = False


def sandbox_tool_argv(  # noqa: C901, PLR0912, PLR0913, PLR0915
    worktree: Path, tool: str, *, pattern: str = '', bwrap: str = 'bwrap'
) -> tuple[str, ...]:
    """Generate only preapproved static commands; reject user-controlled argv."""
    if not worktree.is_absolute() or worktree.is_symlink() or not worktree.is_dir():
        msg = 'Worktree must be an existing absolute real directory'
        raise WorkflowError(msg)
    root = worktree.resolve(strict=True)
    if root in (Path('/'), Path('/home'), Path('/tmp')):  # noqa: S108
        msg = 'Refusing broad sandbox root'
        raise WorkflowError(msg)
    command: tuple[str, ...]
    if tool == 'list-files':
        command = ('/usr/bin/rg', '--files', '--hidden', '-g', '!.git')
    elif tool == 'search-fixed':
        if not pattern or len(pattern) > MAX_PATTERN or '\x00' in pattern or '\n' in pattern:  # noqa: PLR0916
            msg = 'Invalid bounded search pattern'
            raise WorkflowError(msg)
        command = ('/usr/bin/rg', '-n', '--fixed-strings', '-e', pattern, '--', '.')
    elif tool == 'git-status':
        command = ('/usr/bin/git', 'status', '--short', '--untracked-files=no')
    else:
        msg = 'Unknown sandbox tool'
        raise WorkflowError(msg)
    for binary in ('/usr/bin/rg', '/usr/bin/git'):
        if not Path(binary).is_file():
            msg = 'Required sandbox tool unavailable'
            raise WorkflowError(msg)
    argv = [
        bwrap,
        '--die-with-parent',
        '--new-session',
        '--unshare-all',
        '--clearenv',
        '--setenv',
        'PATH',
        '/usr/bin:/bin',
        '--setenv',
        'HOME',
        '/tmp',  # noqa: S108 - isolated tmpfs inside bwrap
        '--setenv',
        'GIT_CONFIG_NOSYSTEM',
        '1',
        '--setenv',
        'GIT_CONFIG_GLOBAL',
        '/dev/null',
        '--setenv',
        'GIT_OPTIONAL_LOCKS',
        '0',
        '--tmpfs',
        '/tmp',  # noqa: S108 - sandbox-private tmpfs mount
        '--proc',
        '/proc',
        '--dev',
        '/dev',
    ]
    for directory in ('/usr', '/bin', '/lib', '/lib64'):
        if Path(directory).exists():
            argv += ['--ro-bind', directory, directory]
    argv += ['--ro-bind', str(root), '/work', '--chdir', '/work', '--', *command]
    return tuple(argv)


def _limit() -> None:
    resource.setrlimit(resource.RLIMIT_CPU, (MAX_SECONDS, MAX_SECONDS))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_OUTPUT, MAX_OUTPUT))
    resource.setrlimit(resource.RLIMIT_AS, (1024 * 1024 * 1024, 1024 * 1024 * 1024))


def run_sandbox_tool(  # noqa: PLR0913
    worktree: Path, tool: str, *, pattern: str = '', authorized: bool = False
) -> ToolResult:
    """No host fallback if Bubblewrap is absent or refuses namespace creation."""
    if not authorized:
        msg = 'Separate operator authorization required for sandbox tool execution'
        raise WorkflowError(msg)
    bwrap = shutil.which('bwrap')
    if bwrap is None:
        msg = 'Bubblewrap unavailable: refusing host execution'
        raise WorkflowError(msg)
    argv = sandbox_tool_argv(worktree, tool, pattern=pattern, bwrap=bwrap)
    with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
        try:
            process = subprocess.run(  # noqa: S603 - restricted static argv
                argv,
                cwd='/',
                env={'PATH': '/usr/bin:/bin'},
                check=False,
                stdout=stdout_file,
                stderr=stderr_file,
                timeout=MAX_SECONDS,
                preexec_fn=_limit,
            )
            code: int | None = process.returncode
            timed_out = False
        except subprocess.TimeoutExpired:
            code = None
            timed_out = True
        stdout_file.seek(0)
        stderr_file.seek(0)
        out = stdout_file.read(MAX_OUTPUT)
        err = stderr_file.read(MAX_OUTPUT)
    if timed_out or err.startswith((b'bwrap:', b'bubblewrap:')):
        msg = 'Sandbox unavailable or timed out; no host fallback'
        raise WorkflowError(msg)
    digest = hashlib.sha256(out + b'\x00' + err).hexdigest()
    return ToolResult(
        tool=tool,
        exit_code=code,
        timed_out=False,
        stdout=out.decode('utf-8', errors='replace'),
        stderr=err.decode('utf-8', errors='replace'),
        output_sha256=digest,
    )


def record_sandbox_tool(result: ToolResult, evidence: Path) -> str:
    """Store an immutable receipt; never overwrite previous or uncertain work."""
    if evidence.is_symlink() or evidence.exists() or not evidence.parent.is_dir():
        msg = 'Evidence destination must be a new path in an existing directory'
        raise WorkflowError(msg)
    payload = (
        json.dumps(
            {
                'tool': result.tool,
                'exit_code': result.exit_code,
                'stdout': result.stdout,
                'stderr': result.stderr,
                'output_sha256': result.output_sha256,
                'sandbox': result.sandbox,
                'host_execution': result.host_execution,
                'timed_out': result.timed_out,
            },
            sort_keys=True,
        )
        + '\n'
    ).encode()
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
    with os.fdopen(os.open(evidence, flags, 0o600), 'wb') as target:
        target.write(payload)
        target.flush()
        os.fsync(target.fileno())
    return hashlib.sha256(payload).hexdigest()
