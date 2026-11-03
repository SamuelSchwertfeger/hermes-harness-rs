"""Fail-closed Bubblewrap functional test gate for reviewed disposable worktrees.

Generated code executes only inside an explicitly approved local sandbox.
No arbitrary shell commands, host fallback, commits, or repair writes.
"""

import hashlib
import json
import os
import re
import resource
import shutil
import subprocess
import tempfile
from pathlib import Path

from hermes_harness.git.worktrees import WorktreeError, _git, audit_worktree
from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.lifecycle import _files, _load

_MAX_OUTPUT = 16_384
_MAX_RECEIPT = 128 * 1024
_TIMEOUT = 90


def _require(condition: bool, message: str) -> None:  # noqa: FBT001 - validation predicate
    if not condition:
        raise WorkflowError(message)


def _sandbox_argv(*, bwrap: str, workspace: Path, python: str) -> list[str]:
    """Build a fixed, network-disabled, no-host-home sandbox command."""
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
        '/tmp',  # noqa: S108 - sandbox-private tmpfs
        '--setenv',
        'PYTHONDONTWRITEBYTECODE',
        '1',
        '--setenv',
        'PYTHONPATH',
        '/work',
        '--tmpfs',
        '/tmp',  # noqa: S108 - sandbox-private tmpfs
        '--proc',
        '/proc',
        '--dev',
        '/dev',
    ]
    for directory in ('/usr', '/bin', '/lib', '/lib64'):
        if Path(directory).exists():
            argv.extend(('--ro-bind', directory, directory))
    argv.extend(('--ro-bind', str(workspace), '/work', '--chdir', '/work'))
    # Use the system interpreter; no host virtualenv or credentials are mounted.
    argv.extend((python, '-m', 'unittest', 'discover', '-s', 'tests', '-p', 'test_*.py'))
    return argv


def _validate_checkpoint(checkpoint: Path, workspace: Path) -> dict[str, object]:
    record, _ = _load(checkpoint)
    _require(record.get('mode') == 'integrity-checkpoint', 'Expected integrity checkpoint')
    _require(record.get('integrity_verified') is True, 'Integrity checkpoint not verified')
    _require(record.get('functional_verified') is False, 'Functional verification already recorded')
    _require(record.get('tests_executed') is False, 'Unexpected prior test execution')
    _require(record.get('human_decision_pending') is True, 'Task not pending decision')
    _require(record.get('workspace') == str(workspace), 'Worktree disagrees with checkpoint')
    _require(
        record.get('next_stage') == 'requires-separate-functional-execution-authorization',
        'Unexpected lifecycle stage',
    )
    return record


def _audit_checkout(  # noqa: C901, PLR0912, PLR0915 - security boundary checks
    root: Path, record: dict[str, object]
) -> dict[str, str]:
    """Revalidate Git identity, ownership and the complete reviewed change set."""
    baseline = record.get('baseline')
    branch = record.get('branch')
    task_id = record.get('task_id')
    if not isinstance(baseline, str) or re.fullmatch(r'[0-9a-f]{40}', baseline) is None:
        msg = 'Checkpoint Git baseline invalid'
        raise WorkflowError(msg)
    if not isinstance(task_id, str) or branch != f'harness/{task_id}':
        msg = 'Checkpoint Git branch invalid'
        raise WorkflowError(msg)
    files = _files(record)
    try:
        actual_branch = _git(root, 'symbolic-ref', '--short', 'HEAD')
        audit = audit_worktree(root, baseline, tuple(files))
    except (WorktreeError, OSError) as exc:
        msg = 'Worktree Git audit failed'
        raise WorkflowError(msg) from exc
    if actual_branch != branch or audit.head != baseline or audit.baseline != baseline:
        msg = 'Worktree Git identity changed since checkpoint'
        raise WorkflowError(msg)
    if set(audit.changed_paths) != set(files):
        msg = 'Worktree changed paths differ from reviewed checkpoint'
        raise WorkflowError(msg)
    for name, digest in files.items():
        target = root / name
        if not target.is_file() or target.is_symlink() or not target.resolve().is_relative_to(root):
            msg = 'Reviewed file is absent or escapes worktree'
            raise WorkflowError(msg)
        if hashlib.sha256(target.read_bytes()).hexdigest() != digest:
            msg = 'Reviewed file changed since checkpoint'
            raise WorkflowError(msg)
    # Git's normal untracked-file audit ignores files hidden by .gitignore.
    # Explicitly reject ignored Python test modules, which unittest could import.
    try:
        ignored = _git(
            root, 'ls-files', '--others', '--ignored', '--exclude-standard', '-z', '--', 'tests'
        )
    except WorktreeError as exc:
        msg = 'Could not inspect ignored test files'
        raise WorkflowError(msg) from exc
    if any(name.endswith('.py') for name in ignored.split('\x00') if name):
        msg = 'Unreviewed ignored Python test file'
        raise WorkflowError(msg)
    return files


def _write_evidence(evidence: Path, result: dict[str, object]) -> str:
    _require(
        not evidence.is_symlink() and not evidence.exists(), 'Evidence path is already occupied'
    )
    _require(
        evidence.parent.is_dir() and not evidence.parent.is_symlink(),
        'Evidence directory is invalid',
    )
    payload = (json.dumps(result, sort_keys=True, indent=2) + '\n').encode()
    _require(len(payload) <= _MAX_RECEIPT, 'Evidence receipt exceeds limit')
    fd = os.open(evidence, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(payload)
    return hashlib.sha256(payload).hexdigest()


def functional_verify(  # noqa: PLR0913, PLR0914, PLR0915 - isolated execution gate
    *,
    checkpoint: Path,
    workspace: Path,
    evidence: Path,
    authorized: bool = False,
) -> dict[str, object]:
    """Run bounded stdlib unittest discovery with no host execution fallback."""
    _require(authorized, 'Separate explicit functional execution authorization required')
    _require(workspace.is_dir() and not workspace.is_symlink(), 'Worktree must be a real directory')
    root = workspace.resolve(strict=True)
    record = _validate_checkpoint(checkpoint, root)
    _require(not evidence.exists() and not evidence.is_symlink(), 'Evidence already exists')
    _require(
        evidence.parent.is_dir() and not evidence.parent.is_symlink(), 'Invalid evidence directory'
    )
    bwrap = shutil.which('bwrap')
    if bwrap is None:
        msg = 'Bubblewrap not installed: refusing host execution'
        raise WorkflowError(msg)
    python = '/usr/bin/python3'
    _require(Path(python).is_file(), 'Sandbox Python unavailable')
    # Must have a dedicated test suite in the reviewed worktree; no arbitrary command argument.
    _require(
        (root / 'tests').is_dir() and not (root / 'tests').is_symlink(),
        'Worktree has no tests directory',
    )
    tests = list((root / 'tests').glob('test_*.py'))
    _require(bool(tests), 'No unittest test_*.py files found')
    _require(all(t.is_file() and not t.is_symlink() for t in tests), 'Symlink or invalid test file')
    # All changes, not only checkpoint-listed hashes, must match accepted review.
    _audit_checkout(root, record)
    argv = _sandbox_argv(bwrap=bwrap, workspace=root, python=python)

    def _limit_output() -> None:
        resource.setrlimit(resource.RLIMIT_FSIZE, (_MAX_OUTPUT, _MAX_OUTPUT))
        resource.setrlimit(resource.RLIMIT_CPU, (_TIMEOUT, _TIMEOUT))
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_AS, (1024 * 1024 * 1024, 1024 * 1024 * 1024))

    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        try:
            completed = subprocess.run(  # noqa: S603 - fixed argv, isolated sandbox
                argv,
                cwd='/',
                env={'PATH': '/usr/bin:/bin'},
                stdout=out,
                stderr=err,
                timeout=_TIMEOUT,
                check=False,
                preexec_fn=_limit_output,
            )
            rc: int | None = completed.returncode
            timed_out = False
        except subprocess.TimeoutExpired:
            rc = None
            timed_out = True
        out.seek(0)
        err.seek(0)
        stdout = out.read(_MAX_OUTPUT).decode(errors='replace')
        stderr = err.read(_MAX_OUTPUT).decode(errors='replace')
    sandbox_started = not stderr.startswith(('bwrap:', 'bubblewrap:')) and not timed_out

    tests_executed = (
        sandbox_started
        and 'Ran ' in stderr
        and (' test in ' in stderr or ' tests in ' in stderr)
        and 'Ran 0 tests' not in stderr
    )

    passed = tests_executed and rc == 0
    result: dict[str, object] = {
        'mode': 'sandboxed-functional-verification',
        'task_id': record['task_id'],
        'baseline': record['baseline'],
        'workspace': str(root),
        'checkpoint_sha256': hashlib.sha256(checkpoint.read_bytes()).hexdigest(),
        'sandbox': 'bubblewrap-unshare-all-no-network',
        'command': ['python3', '-m', 'unittest', 'discover', '-s', 'tests', '-p', 'test_*.py'],
        'tests_executed': tests_executed,
        'generated_code_executed': tests_executed,
        'execution_attempted': True,
        'sandbox_started': sandbox_started,
        'functional_verified': passed,
        'exit_code': rc,
        'timed_out': timed_out,
        'stdout': stdout,
        'stderr': stderr,
        'human_decision_pending': True,
        'committed': False,
        'merged': False,
        'next_stage': (
            'await-human-review'
            if passed
            else 'verification-failed-requires-review'
            if tests_executed
            else 'execution-unconfirmed-requires-review'
        ),
    }
    digest = _write_evidence(evidence, result)
    return {**result, 'evidence': str(evidence), 'evidence_sha256': digest}
