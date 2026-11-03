"""Append-only, bounded Hermes repair coordination (never executes repairs).

The coordinator reserves at most three attempts against one failed functional
receipt. It does not grant inference, worktree writes, code execution or merges.
"""

import hashlib
import json
import os
import re
from typing import TYPE_CHECKING

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.lifecycle import _load

if TYPE_CHECKING:
    from pathlib import Path

_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,63}\Z')
_MAX_ATTEMPTS = 3


MAX_REPAIR_FILES = 16


def _require(condition: bool, message: str) -> None:  # noqa: FBT001 - predicate gate
    if not condition:
        raise WorkflowError(message)


def _write_once(path: Path, value: dict[str, object]) -> str:
    _require(path.parent.is_dir() and not path.parent.is_symlink(), 'Invalid evidence directory')
    payload = (json.dumps(value, indent=2, sort_keys=True) + '\n').encode()
    _require(len(payload) <= 128 * 1024, 'Evidence too large')
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'wb') as stream:
        stream.write(payload)
        stream.flush()
        os.fsync(stream.fileno())
    directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)
    return hashlib.sha256(payload).hexdigest()


def _state_root(directory: Path) -> Path:
    _require(directory.is_dir() and not directory.is_symlink(), 'State directory must exist')
    return directory.resolve(strict=True)


def _read(path: Path) -> dict[str, object]:
    value, _ = _load(path)
    return value


def open_repair(  # noqa: PLR0913 - explicit evidence and authorization boundary
    *, failure: Path, checkpoint: Path, state: Path, authorized: bool = False
) -> dict[str, object]:
    """Open a repair sequence from a qualifying failed sandbox test receipt."""
    _require(authorized, 'Explicit authorization to open repair tracking required')
    root = _state_root(state)
    failed, failed_hash = _load(failure)
    original, checkpoint_hash = _load(checkpoint)
    _require(original.get('mode') == 'integrity-checkpoint', 'Invalid checkpoint')
    _require(failed.get('mode') == 'sandboxed-functional-verification', 'Invalid failure receipt')
    _require(failed.get('functional_verified') is False, 'Functional verification did not fail')
    _require(failed.get('tests_executed') is True, 'Execution unconfirmed; refuse automatic repair')
    _require(failed.get('timed_out') is False, 'Timeout requires operator investigation')
    _require(
        failed.get('sandbox_started') is True, 'Sandbox failure is not repairable test failure'
    )
    _require(failed.get('human_decision_pending') is True, 'Receipt not awaiting a decision')
    _require(failed.get('checkpoint_sha256') == checkpoint_hash, 'Checkpoint digest mismatch')
    for field in ('task_id', 'baseline', 'workspace'):
        _require(failed.get(field) == original.get(field), f'Repair identity mismatch: {field}')
    task_id = original.get('task_id')
    if not isinstance(task_id, str) or _ID.fullmatch(task_id) is None:
        msg = 'Invalid task ID'
        raise WorkflowError(msg)
    _require(original.get('max_repair_attempts') == _MAX_ATTEMPTS, 'Unexpected attempt budget')
    record: dict[str, object] = {
        'mode': 'repair-sequence',
        'task_id': task_id,
        'baseline': original['baseline'],
        'workspace': original['workspace'],
        'checkpoint_sha256': checkpoint_hash,
        'failure_sha256': failed_hash,
        'attempt_limit': _MAX_ATTEMPTS,
        'phase': 'await-attempt-reservation',
        'automatic_execution_authorized': False,
        'human_decision_pending': True,
    }
    destination = root / f'{task_id}-repair-open.json'
    digest = _write_once(destination, record)
    return {**record, 'evidence': str(destination), 'evidence_sha256': digest}


def _sequence(state: Path, task_id: str) -> tuple[Path, dict[str, object]]:
    _require(_ID.fullmatch(task_id) is not None, 'Invalid task identifier')
    root = _state_root(state)
    record = _read(root / f'{task_id}-repair-open.json')
    _require(
        record.get('mode') == 'repair-sequence' and record.get('task_id') == task_id,
        'Invalid repair sequence',
    )
    return root, record


def inspect_repair(*, state: Path, task_id: str) -> dict[str, object]:
    """Read immutable reservations, without replay or dispatch."""
    root, opened = _sequence(state, task_id)
    attempts: list[dict[str, object]] = []
    for number in range(1, _MAX_ATTEMPTS + 1):
        path = root / f'{task_id}-repair-{number}.json'
        if path.exists() or path.is_symlink():
            entry = _read(path)
            _require(entry.get('mode') == 'repair-attempt-reservation', 'Invalid reservation')
            _require(
                entry.get('number') == number and entry.get('task_id') == task_id,
                'Reservation mismatch',
            )
            attempts.append(entry)
        elif attempts:
            _require(
                not any(
                    (root / f'{task_id}-repair-{n}.json').exists()
                    for n in range(number + 1, _MAX_ATTEMPTS + 1)
                ),
                'Noncontiguous attempt history',
            )
    dispositions = _dispositions(root, task_id, attempts)
    terminal = bool(dispositions) and dispositions[-1].get('outcome') in ('passed', 'abandoned')
    pending = bool(attempts) and len(dispositions) < len(attempts)
    return {
        'dispositions': dispositions,
        'terminal': terminal,
        'mode': 'repair-inspection',
        'task_id': task_id,
        'opened': opened,
        'attempts': attempts,
        'attempts_used': len(attempts),
        'remaining': _MAX_ATTEMPTS - len(attempts),
        'next_stage': (
            'finished'
            if terminal
            else 'await-attempt-disposition'
            if pending
            else 'exhausted'
            if len(attempts) == _MAX_ATTEMPTS
            else 'may-reserve-attempt'
        ),
        'automated_dispatch': False,
    }


def reserve_repair(*, state: Path, task_id: str, authorized: bool = False) -> dict[str, object]:
    """Reserve a one-shot attempt; cannot imply inference or execution approval."""
    _require(authorized, 'Explicit authorization to reserve a repair attempt required')
    root, opened = _sequence(state, task_id)
    report = inspect_repair(state=state, task_id=task_id)
    used = report['attempts_used']
    if type(used) is not int or used < 0 or used >= _MAX_ATTEMPTS:
        msg = 'Invalid or exhausted repair attempt count'
        raise WorkflowError(msg)
    _require(
        report['next_stage'] == 'may-reserve-attempt', 'Prior attempt pending or sequence closed'
    )
    number = used + 1
    evidence: dict[str, object] = {
        'mode': 'repair-attempt-reservation',
        'task_id': task_id,
        'number': number,
        'source_failure_sha256': opened['failure_sha256'],
        'checkpoint_sha256': opened['checkpoint_sha256'],
        'stage': 'await-separate-worker-and-reviewer-authorization',
        'worker_dispatched': False,
        'reviewer_dispatched': False,
        'files_applied': False,
        'generated_code_executed': False,
        'human_decision_pending': True,
    }
    path = root / f'{task_id}-repair-{number}.json'
    digest = _write_once(path, evidence)
    return {**evidence, 'evidence': str(path), 'evidence_sha256': digest}


def _dispositions(
    root: Path, task_id: str, attempts: list[dict[str, object]]
) -> list[dict[str, object]]:
    """Read a contiguous, hash-linked history of operator dispositions."""
    completed: list[dict[str, object]] = []
    for number, _reservation in enumerate(attempts, start=1):
        path = root / f'{task_id}-repair-{number}-disposition.json'
        if not path.exists() and not path.is_symlink():
            break
        record = _read(path)
        reserved_path = root / f'{task_id}-repair-{number}.json'
        _, digest = _load(reserved_path)
        _require(
            record.get('mode') == 'repair-attempt-disposition'
            and record.get('task_id') == task_id
            and record.get('number') == number
            and record.get('reservation_sha256') == digest
            and record.get('outcome') in ('rejected', 'failed', 'passed', 'abandoned'),
            'Invalid or unlinked repair disposition',
        )
        _require(
            record.get('automatic_execution_authorized') is False,
            'Repair disposition must not authorize execution',
        )
        completed.append(record)
    _require(
        not any(
            (root / f'{task_id}-repair-{n}-disposition.json').exists()
            for n in range(len(completed) + 1, len(attempts) + 1)
        ),
        'Noncontiguous repair disposition history',
    )
    _require(
        not any(entry['outcome'] in ('passed', 'abandoned') for entry in completed[:-1]),
        'Terminal repair attempt followed by another disposition',
    )
    return completed


def record_repair_disposition(  # noqa: PLR0913, PLR0914, PLR0915 - bounded evidence and authorization boundary
    *,
    state: Path,
    task_id: str,
    outcome: str,
    review: Path | None = None,
    verification: Path | None = None,
    authorized: bool = False,
) -> dict[str, object]:
    """Record an operator-reviewed attempt outcome; never apply or execute changes.

    Rejected and abandoned attempts require no generated-code evidence. Failed
    and passed outcomes require a separately generated sandbox receipt.
    """
    _require(authorized, 'Explicit authorization to record disposition required')
    _require(outcome in ('rejected', 'failed', 'passed', 'abandoned'), 'Unknown repair outcome')
    root, opened = _sequence(state, task_id)
    report = inspect_repair(state=state, task_id=task_id)
    _require(report['next_stage'] == 'await-attempt-disposition', 'No pending attempt')
    used = report['attempts_used']
    if not isinstance(used, int) or used < 1:
        msg = 'Malformed attempt count'
        raise WorkflowError(msg)
    reservation_path = root / f'{task_id}-repair-{used}.json'
    _, reservation_hash = _load(reservation_path)
    review_hash: str | None = None
    verification_hash: str | None = None
    if outcome in ('rejected', 'failed', 'passed'):
        if review is None:
            msg = 'Independent review evidence required'
            raise WorkflowError(msg)
        reviewed, review_hash = _load(review)
        # Only the one-shot native dispatch receipt may settle a reviewed attempt.
        native_review = root / f'{task_id}-repair-{used}-review.json'
        claim_path = root / f'{task_id}-repair-{used}-dispatch.json'
        _require(
            review.resolve() == native_review.resolve()
            and claim_path.is_file()
            and not claim_path.is_symlink(),
            'Review must originate from native repair dispatch',
        )
        claim, claim_hash = _load(claim_path)
        _require(
            claim.get('mode') == 'repair-dispatch-claim'
            and claim.get('task_id') == task_id
            and claim.get('number') == used
            and claim.get('reservation_sha256') == reservation_hash
            and reviewed.get('claim_sha256') == claim_hash
            and reviewed.get('checkpoint_sha256') == opened['checkpoint_sha256']
            and reviewed.get('source_failure_sha256') == opened['failure_sha256']
            and reviewed.get('baseline') == opened['baseline']
            and reviewed.get('workspace') == opened['workspace']
            and reviewed.get('owned_paths') == claim.get('owned_paths')
            and reviewed.get('worker_model') == claim.get('worker_model')
            and reviewed.get('reviewer_model') == claim.get('reviewer_model'),
            'Review dispatch provenance mismatch',
        )
        replacements = reviewed.get('replacements')
        preimages = claim.get('preimages')
        owned = claim.get('owned_paths')
        if (
            not isinstance(replacements, list)
            or not isinstance(preimages, dict)
            or not isinstance(owned, list)
        ):
            msg = 'Malformed reviewed replacement evidence'
            raise WorkflowError(msg)
        _require(
            isinstance(replacements, list)
            and 1 <= len(replacements) <= MAX_REPAIR_FILES
            and isinstance(preimages, dict)
            and isinstance(owned, list),
            'Malformed reviewed replacement evidence',
        )
        seen: set[str] = set()
        for replacement in replacements:
            _require(isinstance(replacement, dict), 'Malformed reviewed replacement')
            name = replacement.get('path')
            content = replacement.get('content')
            expected = replacement.get('expected_sha256')
            digest = replacement.get('content_sha256')
            _require(
                isinstance(name, str)
                and name in owned
                and name not in seen
                and isinstance(content, str)
                and len(content.encode()) <= 48 * 1024
                and isinstance(expected, str)
                and preimages.get(name) == expected
                and isinstance(digest, str)
                and hashlib.sha256(content.encode()).hexdigest() == digest,
                'Invalid reviewed replacement provenance',
            )
            seen.add(name)
        _require(
            reviewed.get('mode') == 'repair-independent-review'
            and reviewed.get('task_id') == task_id
            and reviewed.get('number') == used
            and reviewed.get('reservation_sha256') == reservation_hash
            and reviewed.get('worker_model') != reviewed.get('reviewer_model')
            and isinstance(reviewed.get('worker_model'), str)
            and isinstance(reviewed.get('reviewer_model'), str)
            and reviewed.get('verdict')
            == ('changes_required' if outcome == 'rejected' else 'accept'),
            'Review evidence is not bound to this attempt',
        )
    if outcome in ('failed', 'passed'):
        if verification is None:
            msg = 'Functional verification evidence required'
            raise WorkflowError(msg)
        verified, verification_hash = _load(verification)
        _require(
            verified.get('mode') == 'sandboxed-functional-verification'
            and verified.get('task_id') == task_id
            and verified.get('tests_executed') is True
            and verified.get('sandbox_started') is True
            and verified.get('timed_out') is False
            and verified.get('functional_verified') is (outcome == 'passed')
            and verified.get('checkpoint_sha256') == opened.get('checkpoint_sha256')
            and verified.get('baseline') == opened.get('baseline')
            and verified.get('workspace') == opened.get('workspace'),
            'Verification evidence is not bound to repair sequence',
        )
    _require(
        outcome != 'abandoned' or review is None,
        'Abandoned attempt must not attach a review',
    )
    _require(
        outcome not in ('rejected', 'abandoned') or verification is None,
        'Unexpected verification evidence',
    )
    evidence: dict[str, object] = {
        'mode': 'repair-attempt-disposition',
        'task_id': task_id,
        'number': used,
        'reservation_sha256': reservation_hash,
        'review_sha256': review_hash,
        'verification_sha256': verification_hash,
        'outcome': outcome,
        'automatic_execution_authorized': False,
        'human_decision_pending': True,
        'next_stage': (
            'finished'
            if outcome in ('passed', 'abandoned')
            else 'exhausted'
            if used == _MAX_ATTEMPTS
            else 'may-reserve-attempt'
        ),
    }
    destination = root / f'{task_id}-repair-{used}-disposition.json'
    digest = _write_once(destination, evidence)
    return {**evidence, 'evidence': str(destination), 'evidence_sha256': digest}
