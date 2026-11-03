"""Pure, fail-closed checks for supervised sandbox argv and service recovery.

No privileged commands, provider calls, or mutation of a service ledger.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from collections.abc import Sequence


class OperatorSafetyError(RuntimeError):
    """A proposed sandbox does not establish its claimed role containment."""


@dataclass(frozen=True, slots=True)
class MountEvidence:
    """Exact role mount sources and destinations extracted from argv."""

    writable: tuple[tuple[str, str], ...]
    readonly: tuple[tuple[str, str], ...]


_MOUNT_FLAGS = ('--bind', '--ro-bind', '--dev-bind')
_REQUIRED_FLAGS = ('--unshare-net', '--unshare-pid', '--unshare-user', '--die-with-parent')


def _mount_decl(argv: Sequence[str], cursor: int) -> tuple[str, str]:
    if cursor + 2 >= len(argv):
        msg = 'Incomplete bubblewrap mount declaration'
        raise OperatorSafetyError(msg)
    source, destination = argv[cursor + 1 : cursor + 3]
    if not source.startswith('/') or not destination.startswith('/'):
        msg = 'Mount must use absolute source and destination'
        raise OperatorSafetyError(msg)
    return source, destination


def extract_mounts(argv: Sequence[str]) -> MountEvidence:
    """Parse bwrap mount triples without interpreting payload arguments as options."""
    if argv.count('/usr/bin/bwrap') != 1:
        msg = 'Exactly one explicit bubblewrap executable required'
        raise OperatorSafetyError(msg)
    start = argv.index('/usr/bin/bwrap') + 1
    writable: list[tuple[str, str]] = []
    readonly: list[tuple[str, str]] = []
    cursor = start
    while cursor < len(argv):
        option = argv[cursor]
        if option == '--chdir':
            break
        if option not in _MOUNT_FLAGS:
            cursor += 1
            continue
        pair = _mount_decl(argv, cursor)
        if option == '--ro-bind':
            readonly.append(pair)
        else:
            writable.append(pair)
        cursor += 3
    return MountEvidence(writable=tuple(writable), readonly=tuple(readonly))


def _check_role_mounts(mounts: MountEvidence, role: str) -> None:
    work_writable = [pair for pair in mounts.writable if pair[1] == '/workspace']
    work_readonly = [pair for pair in mounts.readonly if pair[1] == '/workspace']
    evidence_readonly = [pair for pair in mounts.readonly if pair[1] == '/evidence']
    if len(evidence_readonly) != 1 or any(pair[1] == '/evidence' for pair in mounts.writable):
        msg = 'Evidence must have one read-only source'
        raise OperatorSafetyError(msg)
    if role == 'worker':
        if len(work_writable) != 1 or work_readonly:
            msg = 'Worker workspace must have one writable source'
            raise OperatorSafetyError(msg)
    elif len(work_readonly) != 1 or work_writable or mounts.writable:
        msg = 'Reviewer must have one read-only workspace and no writable bind mounts'
        raise OperatorSafetyError(msg)


def validate_role_sandbox(argv: Sequence[str], *, role: str) -> MountEvidence:
    """Require role-specific mounts and namespace isolation before role admission."""
    if role not in ('worker', 'reviewer'):
        msg = 'Unknown sandbox role'
        raise OperatorSafetyError(msg)
    mounts = extract_mounts(argv)
    options = argv[argv.index('/usr/bin/bwrap') + 1 :]
    if '--chdir' in options:
        options = options[: options.index('--chdir')]
    if any(flag not in options for flag in _REQUIRED_FLAGS):
        msg = 'Required sandbox isolation missing'
        raise OperatorSafetyError(msg)
    _check_role_mounts(mounts, role)
    sources = [source for source, _ in (*mounts.writable, *mounts.readonly)]
    if len(sources) != len(set(sources)):
        msg = 'Mount sources must not be reused across sandbox destinations'
        raise OperatorSafetyError(msg)
    return mounts


def validate_separate_roles(worker_argv: Sequence[str], reviewer_argv: Sequence[str]) -> None:
    """Ensure reviewer never mounts the worker's writable source or shared evidence as writable."""
    worker = validate_role_sandbox(worker_argv, role='worker')
    reviewer = validate_role_sandbox(reviewer_argv, role='reviewer')
    if any(source == worker.writable[0][0] for source, _ in reviewer.readonly):
        msg = 'Reviewer must not receive the worker writable workspace'
        raise OperatorSafetyError(msg)


# Temporary exports for legacy module import compatibility.
__all__ = [
    '_MOUNT_FLAGS',
    '_REQUIRED_FLAGS',
    'MountEvidence',
    'OperatorSafetyError',
    '_check_role_mounts',
    '_mount_decl',
    'dataclass',
    'extract_mounts',
    'validate_role_sandbox',
    'validate_separate_roles',
]
