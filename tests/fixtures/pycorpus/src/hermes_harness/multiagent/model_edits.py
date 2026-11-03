"""Validate model-produced file proposals without applying them.

A proposal is data, not a command. It cannot rename, delete, execute, commit,
or mutate a worktree. Applying changes requires a separate qualified boundary.
"""

import hashlib
import json
from dataclasses import dataclass
from pathlib import PurePosixPath

from hermes_harness.multiagent.runtime import WorkflowError

MAX_PROPOSAL_BYTES = 128 * 1024
MAX_FILE_BYTES = 48 * 1024
MAX_FILES = 16


@dataclass(frozen=True, slots=True)
class ProposedFile:
    """Model-proposed UTF-8 file replacement under assigned ownership."""

    path: str
    content: str
    sha256: str


@dataclass(frozen=True, slots=True)
class EditProposal:
    task_id: str
    files: tuple[ProposedFile, ...]


def _owned(name: str, scopes: tuple[str, ...]) -> bool:
    return any(name == scope or name.startswith(f'{scope}/') for scope in scopes)


def _valid_owned_path(value: str, scopes: tuple[str, ...]) -> bool:
    parts = value.split('/')
    if not value or PurePosixPath(value).is_absolute():
        return False
    if '\\' in value or '\x00' in value:
        return False
    if any(piece in ('', '.', '..') for piece in parts) or parts[0] == '.git':
        return False
    return _owned(value, scopes)


def _safe_path(value: object, scopes: tuple[str, ...]) -> str:
    if not isinstance(value, str):
        msg = 'Proposal path must be a string'
        raise WorkflowError(msg)
    if not _valid_owned_path(value, scopes):
        msg = 'Proposal touches a forbidden or unowned path'
        raise WorkflowError(msg)
    return value


def _parse_file_row(row: object, owned_paths: tuple[str, ...], seen: set[str]) -> ProposedFile:
    if not isinstance(row, dict) or set(row) != {'path', 'content'}:
        msg = 'Invalid proposed file schema'
        raise WorkflowError(msg)
    name = _safe_path(row['path'], owned_paths)
    content = row['content']
    if not isinstance(content, str) or len(content.encode('utf-8')) > MAX_FILE_BYTES:
        msg = 'Invalid proposed file content'
        raise WorkflowError(msg)
    if name in seen:
        msg = 'Duplicate proposed path'
        raise WorkflowError(msg)
    seen.add(name)
    return ProposedFile(name, content, hashlib.sha256(content.encode('utf-8')).hexdigest())


def parse_edit_proposal(payload: str, task_id: str, owned_paths: tuple[str, ...]) -> EditProposal:
    """Admit a bounded file proposal; no filesystem access or network calls."""
    if len(payload.encode('utf-8')) > MAX_PROPOSAL_BYTES:
        msg = 'Oversized model proposal'
        raise WorkflowError(msg)
    try:
        document: object = json.loads(payload)
    except (ValueError, UnicodeError) as exc:
        msg = 'Malformed model proposal'
        raise WorkflowError(msg) from exc
    if not isinstance(document, dict) or set(document) != {'task_id', 'files'}:
        msg = 'Invalid proposal schema'
        raise WorkflowError(msg)
    if document['task_id'] != task_id:
        msg = 'Proposal task identity mismatch'
        raise WorkflowError(msg)
    rows = document['files']
    if not isinstance(rows, list) or not 1 <= len(rows) <= MAX_FILES:
        msg = 'Invalid proposal file count'
        raise WorkflowError(msg)
    seen: set[str] = set()
    files = tuple(_parse_file_row(row, owned_paths, seen) for row in rows)
    return EditProposal(task_id, files)
