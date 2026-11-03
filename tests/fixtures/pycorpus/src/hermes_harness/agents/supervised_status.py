"""Read-only CLI for retained offline supervised execution evidence."""

import argparse
import json
import sqlite3
from pathlib import Path


def inspect(root: Path, task_id: str) -> dict[str, object]:
    """Read immutable records without creating a ledger or touching the gateway."""
    state = root / 'state' / task_id
    ledger = root / 'admissions.sqlite'
    if root.is_symlink() or state.is_symlink() or ledger.is_symlink():
        msg = 'Symlinked state or ledger is not permitted'
        raise ValueError(msg)
    files = (
        'reservation.json',
        'brief.json',
        'worker.json',
        'worktree-audit.json',
        'review.json',
        'approval-request.json',
    )
    evidence: dict[str, bool] = {}
    for name in files:
        path = state / name
        if path.is_symlink():
            msg = 'Symlinked evidence record is not permitted'
            raise ValueError(msg)
        evidence[name] = path.is_file()
    admissions: list[dict[str, str | None]] = []
    if ledger.is_file():
        with sqlite3.connect(f'file:{ledger}?mode=ro', uri=True) as connection:
            for submission_id, session_id, admission_id, local_state in connection.execute(
                'SELECT submission_id, session_id, admission_id, local_state '
                'FROM admissions ORDER BY submission_id LIMIT 100'
            ):
                admissions.append({
                    'submission_id': submission_id,
                    'session_id': session_id,
                    'admission_id': admission_id,
                    'state': local_state,
                })
    approved: bool | None = None
    approval_path = state / 'approval-request.json'
    if evidence['approval-request.json']:
        content: object = json.loads(approval_path.read_text())
        if not isinstance(content, dict) or type(content.get('approved')) is not bool:
            msg = 'Invalid approval-request evidence'
            raise ValueError(msg)
        approved = content['approved']
    return {
        'task_id': task_id,
        'evidence': evidence,
        'admissions': admissions,
        'approved': approved,
    }


def main() -> None:
    """Print evidence status as JSON, with no RPC or write side effects."""
    parser = argparse.ArgumentParser(description='Read-only offline supervised status')
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--task-id', default='scripted-demo')
    args = parser.parse_args()
    print(json.dumps(inspect(args.root, args.task_id), indent=2))


if __name__ == '__main__':
    main()

# Temporary exports for legacy module import compatibility.
__all__ = [
    'Path',
    'argparse',
    'inspect',
    'json',
    'main',
    'sqlite3',
]
