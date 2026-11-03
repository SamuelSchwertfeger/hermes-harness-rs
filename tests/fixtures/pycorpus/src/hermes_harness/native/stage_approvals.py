"""Immutable approval checkpoints for four Hermes-native workflow stages."""

import hashlib
import json
import re
from pathlib import Path

from hermes_harness.multiagent.runtime import WorkflowError, _write_once
from hermes_harness.native.workflows import _git, repository

STAGES = ('lets-investigate', 'what-we-know', 'agents-assemble', 'stick-the-landing')
ID = re.compile(r'[a-z0-9][a-z0-9-]{0,63}\Z')
MAX_EVIDENCE = 262144
MAX_SESSION_LENGTH = 128


def _directory(state: Path, run_id: str) -> Path:
    root = state / run_id
    if not ID.fullmatch(run_id) or root.is_symlink() or not root.is_dir():
        msg = 'Invalid or missing workflow'
        raise WorkflowError(msg)
    return root


def _read(path: Path) -> dict[str, object]:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_EVIDENCE:
        msg = 'Missing or untrusted workflow record'
        raise WorkflowError(msg)
    value: object = json.loads(path.read_text(encoding='utf-8'))
    if not isinstance(value, dict):
        msg = 'Invalid workflow record'
        raise WorkflowError(msg)
    return value


def begin(  # noqa: PLR0913, PLR0917
    state: Path, run_id: str, repo: Path, session: str
) -> dict[str, object]:
    """Bind a new workflow to one clean repository HEAD and Hermes session."""
    if not ID.fullmatch(run_id) or not session.strip() or len(session) > MAX_SESSION_LENGTH:
        msg = 'Invalid workflow or session identity'
        raise WorkflowError(msg)
    if state.is_symlink() or not state.is_dir():
        msg = 'Pre-existing real state directory required'
        raise WorkflowError(msg)
    root = repository(repo)
    if _git(root, 'status', '--porcelain'):
        msg = 'Refusing workflow on dirty source checkout'
        raise WorkflowError(msg)
    directory = state / run_id
    directory.mkdir(mode=0o700, exist_ok=False)
    origin: dict[str, object] = {
        'run_id': run_id,
        'repository': str(root),
        'head': _git(root, 'rev-parse', 'HEAD').strip(),
        'session': session,
    }
    _write_once(directory / 'origin.json', origin)
    return snapshot(state, run_id)


def snapshot(state: Path, run_id: str) -> dict[str, object]:
    directory = _directory(state, run_id)
    origin = _read(directory / 'origin.json')
    approved: list[str] = []
    for index, stage in enumerate(STAGES):
        evidence_path = directory / f'{index:02d}-evidence.json'
        grant_path = directory / f'{index:02d}-approval.json'
        if not evidence_path.exists():
            return {
                **origin,
                'next_stage': stage,
                'approval_pending': False,
                'approved_stages': approved,
            }
        evidence = _read(evidence_path)
        if evidence.get('stage') != stage:
            msg = 'Evidence stage mismatch'
            raise WorkflowError(msg)
        if not grant_path.exists():
            return {
                **origin,
                'next_stage': stage,
                'approval_pending': True,
                'evidence': evidence,
                'approved_stages': approved,
            }
        grant = _read(grant_path)
        if grant.get('sha256') != evidence.get('sha256') or grant.get('stage') != stage:
            msg = 'Evidence approval mismatch'
            raise WorkflowError(msg)
        approved.append(stage)
    return {
        **origin,
        'next_stage': None,
        'approval_pending': False,
        'approved_stages': approved,
        'finished': True,
    }


def submit(  # noqa: PLR0913, PLR0917
    state: Path,
    run_id: str,
    stage: str,
    evidence: Path,
    session: str,
) -> dict[str, object]:
    current = snapshot(state, run_id)
    if current['next_stage'] != stage or current['approval_pending']:
        msg = 'Stage out of order or awaiting approval'
        raise WorkflowError(msg)
    if session != current['session']:
        msg = 'Session mismatch'
        raise WorkflowError(msg)
    if not evidence.is_file() or evidence.is_symlink() or evidence.stat().st_size > MAX_EVIDENCE:
        msg = 'Bounded stage evidence file required'
        raise WorkflowError(msg)
    source = repository(Path(str(current['repository'])))
    if _git(source, 'rev-parse', 'HEAD').strip() != current['head']:
        msg = 'Pinned Git HEAD changed'
        raise WorkflowError(msg)
    digest = hashlib.sha256(evidence.read_bytes()).hexdigest()
    index = STAGES.index(stage)
    _write_once(
        _directory(state, run_id) / f'{index:02d}-evidence.json',
        {
            'stage': stage,
            'sha256': digest,
            'path': str(evidence.resolve()),
            'session': session,
        },
    )
    return snapshot(state, run_id)


def approve(  # noqa: PLR0913, PLR0917
    state: Path,
    run_id: str,
    stage: str,
    digest: str,
    session: str,
    *,
    operator_confirmed: bool = False,
) -> dict[str, object]:
    """Caller must independently verify the operator's specific affirmative grant."""
    if not operator_confirmed:
        msg = 'Explicit operator confirmation required'
        raise WorkflowError(msg)
    current = snapshot(state, run_id)
    evidence = current.get('evidence')
    mismatch = (
        not current['approval_pending'],
        stage != current['next_stage'],
        session != current['session'],
        not isinstance(evidence, dict),
        not isinstance(evidence, dict) or digest != evidence.get('sha256'),
    )
    if any(mismatch):
        msg = 'Stage, session or evidence digest mismatch'
        raise WorkflowError(msg)
    index = STAGES.index(stage)
    _write_once(
        _directory(state, run_id) / f'{index:02d}-approval.json',
        {
            'stage': stage,
            'sha256': digest,
            'session': session,
            'operator_confirmed': True,
        },
    )
    return snapshot(state, run_id)
