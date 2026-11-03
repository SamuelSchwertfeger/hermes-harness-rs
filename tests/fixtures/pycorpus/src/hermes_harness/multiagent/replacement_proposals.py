"""Admit model-proposed replacements with explicit expected preimage hashes.

No file is read or modified while parsing. Execution remains a separate,
operator-authorized disposable-worktree operation.
"""

import json
import re
from pathlib import PurePosixPath

from hermes_harness.multiagent.replace_edits import FileReplacement
from hermes_harness.multiagent.runtime import WorkflowError

MAX_REQUEST_BYTES = 128 * 1024
MAX_REPLACEMENTS = 16
MAX_REPLACEMENT_BYTES = 48 * 1024
SHA256 = re.compile(r'[0-9a-f]{64}\Z')


def _reject_duplicates(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            msg = 'Duplicate replacement JSON field'
            raise WorkflowError(msg)
        result[key] = value
    return result


def _valid_replacement_path(name: str, owned_paths: tuple[str, ...], seen: set[str]) -> bool:
    parts = name.split('/')
    if not name or PurePosixPath(name).is_absolute():
        return False
    if '\\' in name or '\x00' in name:
        return False
    if any(part in ('', '.', '..') for part in parts) or parts[0] == '.git':
        return False
    if name in seen:
        return False
    return any(name == own or name.startswith(f'{own}/') for own in owned_paths)


def _replacement_row(row: object, owned_paths: tuple[str, ...], seen: set[str]) -> FileReplacement:
    if not isinstance(row, dict) or set(row) != {'path', 'expected_sha256', 'content'}:
        msg = 'Invalid replacement entry schema'
        raise WorkflowError(msg)
    name, digest, content = row['path'], row['expected_sha256'], row['content']
    if not isinstance(name, str):
        msg = 'Invalid replacement path'
        raise WorkflowError(msg)
    if not _valid_replacement_path(name, owned_paths, seen):
        msg = 'Unsafe, duplicate or unowned replacement path'
        raise WorkflowError(msg)
    if not isinstance(digest, str) or SHA256.fullmatch(digest) is None:
        msg = 'Invalid expected preimage SHA-256'
        raise WorkflowError(msg)
    if not isinstance(content, str) or len(content.encode('utf-8')) > MAX_REPLACEMENT_BYTES:
        msg = 'Invalid replacement content'
        raise WorkflowError(msg)
    seen.add(name)
    return FileReplacement(name, digest, content)


def parse_replacement_proposal(
    payload: str, task_id: str, owned_paths: tuple[str, ...]
) -> tuple[FileReplacement, ...]:
    """Reject ambiguous or unowned replacements before reading worktree data."""
    if len(payload.encode('utf-8')) > MAX_REQUEST_BYTES:
        msg = 'Oversized replacement proposal'
        raise WorkflowError(msg)
    try:
        document: object = json.loads(payload, object_pairs_hook=_reject_duplicates)
    except (ValueError, UnicodeError) as exc:
        msg = 'Malformed replacement proposal JSON'
        raise WorkflowError(msg) from exc
    if not isinstance(document, dict) or set(document) != {'task_id', 'replacements'}:
        msg = 'Invalid replacement proposal schema'
        raise WorkflowError(msg)
    if document['task_id'] != task_id:
        msg = 'Replacement task mismatch'
        raise WorkflowError(msg)
    rows = document['replacements']
    if not isinstance(rows, list) or not 1 <= len(rows) <= MAX_REPLACEMENTS:
        msg = 'Invalid replacement count'
        raise WorkflowError(msg)
    seen: set[str] = set()
    return tuple(_replacement_row(row, owned_paths, seen) for row in rows)
