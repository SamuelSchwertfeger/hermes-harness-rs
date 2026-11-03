"""Bounded Git worktree lifecycle and path-ownership audit.

This is a Git filesystem boundary, not an OS security sandbox. Untrusted
agents must run in an independently restricted container or account.
"""

import os
import re
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path, PurePosixPath


@dataclass(frozen=True, slots=True)
class WorktreeSpec:
    ticket_id: str
    branch: str
    owned_paths: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class WorktreeAudit:
    changed_paths: tuple[str, ...]
    head: str
    baseline: str


class WorktreeError(RuntimeError):
    """Git or ownership boundary violation."""


def _git(repo: Path, *args: str) -> str:
    git_executable = shutil.which('git')
    if git_executable is None:
        msg = 'Git executable not found'
        raise WorktreeError(msg)
    try:
        done = subprocess.run(  # noqa: S603
            [git_executable, '-C', str(repo), *args],
            check=True,
            capture_output=True,
            timeout=15,
            env={
                'PATH': os.environ.get('PATH', '/usr/bin:/bin'),
                'HOME': str(repo),
                'GIT_CONFIG_NOSYSTEM': '1',
                'GIT_TERMINAL_PROMPT': '0',
            },
        )
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        msg = f'Git operation failed: {args[0]}'
        raise WorktreeError(msg) from exc
    return done.stdout.decode('utf-8', errors='strict').strip()


def _validate_relative(path: str) -> None:
    value = PurePosixPath(path)
    invalid_components = any(component in {'..', '.'} for component in path.split('/'))
    invalid_chars = '\\' in path or '\x00' in path
    invalid_path = not path or value.is_absolute()

    if invalid_path or invalid_components or invalid_chars:
        msg = f'Invalid relative path: {path!r}'
        raise WorktreeError(msg)


def _validate_spec(spec: WorktreeSpec) -> None:
    if re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', spec.ticket_id) is None:
        msg = 'Invalid ticket ID'
        raise WorktreeError(msg)
    if not spec.branch.startswith('harness/') or spec.branch != f'harness/{spec.ticket_id}':
        msg = 'Branch must match harness/<ticket-id>'
        raise WorktreeError(msg)
    if not spec.owned_paths:
        msg = 'At least one owned path is required'
        raise WorktreeError(msg)
    for path in spec.owned_paths:
        _validate_relative(path)


def create_worktree(repo: Path, workspace: Path, spec: WorktreeSpec) -> Path:
    """Create a worktree at a new path; never remove or overwrite existing data."""
    _validate_spec(spec)
    repo = repo.resolve(strict=True)
    workspace = workspace.resolve(strict=False)
    if workspace.is_relative_to(repo) or repo.is_relative_to(workspace):
        msg = 'Workspace must be disjoint from repository'
        raise WorktreeError(msg)
    if workspace.exists() or workspace.is_symlink():
        msg = 'Workspace already exists'
        raise WorktreeError(msg)
    if _git(repo, 'rev-parse', '--show-toplevel') != str(repo):
        msg = 'Repository path must be the Git root'
        raise WorktreeError(msg)
    if _git(repo, 'status', '--porcelain'):
        msg = 'Refusing to create worktree from dirty source repository'
        raise WorktreeError(msg)
    _git(repo, 'worktree', 'add', '-b', spec.branch, str(workspace), 'HEAD')
    return workspace


def _changed_paths(repo: Path, baseline: str) -> list[str]:
    """Track committed, staged, unstaged and untracked paths, including deletions."""
    command_sets = (
        ('diff', '--name-only', '-z', baseline, 'HEAD'),
        ('diff', '--name-only', '-z'),
        ('diff', '--cached', '--name-only', '-z'),
        ('ls-files', '--others', '--exclude-standard', '-z'),
    )
    names: set[str] = set()
    for command in command_sets:
        raw = _git(repo, *command)
        names.update(name for name in raw.split('\x00') if name)
    return sorted(names)


def audit_worktree(worktree: Path, baseline: str, owned_paths: tuple[str, ...]) -> WorktreeAudit:
    """Fail closed on ownership violations and symlinks in changed path ancestry."""
    root = worktree.resolve(strict=True)
    if not owned_paths:
        msg = 'Missing owned paths'
        raise WorktreeError(msg)
    for owned in owned_paths:
        _validate_relative(owned)
    # Never interpolate baseline into a shell; verify it resolves to a commit.
    base = _git(root, 'rev-parse', '--verify', '--end-of-options', f'{baseline}^{{commit}}')
    head = _git(root, 'rev-parse', 'HEAD')
    changed = _changed_paths(root, base)
    for name in changed:
        _validate_relative(name)
        if not any(name == own or name.startswith(f'{own}/') for own in owned_paths):
            msg = f'Path outside ownership: {name}'
            raise WorktreeError(msg)
        candidate = root / name
        if not candidate.resolve(strict=False).is_relative_to(root):
            msg = f'Path escapes worktree: {name}'
            raise WorktreeError(msg)
        current = candidate
        while current != root:
            if current.is_symlink():
                msg = f'Symlink touched: {name}'
                raise WorktreeError(msg)
            current = current.parent
    return WorktreeAudit(tuple(changed), head, base)


def remove_worktree(repo: Path, worktree: Path, *, expected_branch: str) -> None:
    """Remove only an explicitly named clean worktree; retain its branch for audit."""
    root = worktree.resolve(strict=True)
    branch = _git(root, 'symbolic-ref', '--short', 'HEAD')
    if branch != expected_branch or not branch.startswith('harness/'):
        msg = 'Refusing removal of unexpected worktree branch'
        raise WorktreeError(msg)
    if _git(root, 'status', '--porcelain'):
        msg = 'Refusing removal of dirty worktree'
        raise WorktreeError(msg)
    _git(repo, 'worktree', 'remove', str(root))


# Temporary exports for legacy module import compatibility.
__all__ = [
    'Path',
    'PurePosixPath',
    'WorktreeAudit',
    'WorktreeError',
    'WorktreeSpec',
    '_changed_paths',
    '_git',
    '_validate_relative',
    '_validate_spec',
    'audit_worktree',
    'create_worktree',
    'dataclass',
    'os',
    're',
    'remove_worktree',
    'shutil',
    'subprocess',
]
