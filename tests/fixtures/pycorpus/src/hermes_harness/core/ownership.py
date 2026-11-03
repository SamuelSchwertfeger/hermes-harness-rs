"""Verify working tree changes are confined to an approved set of paths."""

from pathlib import Path, PurePosixPath


def check_paths(changed: list[str], allowed: list[str]) -> None:
    for name in changed:
        path = PurePosixPath(name)
        if path.is_absolute() or '..' in path.parts or not name:
            msg = f'Unsafe changed path: {name}'
            raise PermissionError(msg)
        if not any(name == root or name.startswith(root.rstrip('/') + '/') for root in allowed):
            msg = f'Unowned changed path: {name}'
            raise PermissionError(msg)


def check_symlink_boundaries(repo: Path, changed: list[str]) -> None:
    """Reject any touched path routed through a symlink outside the repository."""
    root = repo.resolve(strict=True)
    for name in changed:
        candidate = repo / name
        if not candidate.resolve(strict=False).is_relative_to(root):
            msg = f'Changed path escapes repository: {name}'
            raise PermissionError(msg)


# Temporary exports for legacy module import compatibility.
__all__ = [
    'Path',
    'PurePosixPath',
    'check_paths',
    'check_symlink_boundaries',
]
