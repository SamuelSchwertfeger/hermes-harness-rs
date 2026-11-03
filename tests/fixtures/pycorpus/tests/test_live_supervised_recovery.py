"""Offline recovery and safety invariants; no gateway or provider invoked."""

import hashlib
import json
from typing import TYPE_CHECKING

import pytest

from hermes_harness.supervised.live_supervised_recovery import (
    Phase,
    RecoveryError,
    RecoveryIdentity,
    RecoveryLedger,
    checkpoint_pending_human,
    evidence_digest,
    inspect_pending_decision,
)

if TYPE_CHECKING:
    from pathlib import Path


@pytest.fixture
def identity() -> RecoveryIdentity:
    return RecoveryIdentity(
        task_id='task', worker_submission_id='worker', reviewer_submission_id='reviewer'
    )


def test_restart_is_idempotent_but_no_replay(tmp_path: Path, identity: RecoveryIdentity) -> None:
    path = tmp_path / 'state.db'
    ledger = RecoveryLedger(path)
    assert ledger.initialize(identity).phase == Phase.PLANNED
    digest = evidence_digest({'worker_admission': 'w-1'})
    assert ledger.advance(identity, (Phase.PLANNED, Phase.WORKER_ADMITTED), digest).phase == (
        Phase.WORKER_ADMITTED
    )
    restarted = RecoveryLedger(path)
    assert restarted.initialize(identity).phase == Phase.WORKER_ADMITTED
    with pytest.raises(RecoveryError, match='Stale checkpoint'):
        restarted.advance(identity, (Phase.PLANNED, Phase.WORKER_ADMITTED), digest)


def test_identity_collision_rejected(tmp_path: Path, identity: RecoveryIdentity) -> None:
    ledger = RecoveryLedger(tmp_path / 'state.db')
    ledger.initialize(identity)
    other = RecoveryIdentity(
        task_id='task', worker_submission_id='other', reviewer_submission_id='reviewer'
    )
    with pytest.raises(RecoveryError, match='identity'):
        ledger.initialize(other)


@pytest.mark.parametrize('reason', ['operator denied', 'cancelled', 'gateway uncertain'])
def test_halt_freezes_recovery(tmp_path: Path, identity: RecoveryIdentity, reason: str) -> None:
    ledger = RecoveryLedger(tmp_path / 'state.db')
    ledger.initialize(identity)
    assert ledger.halt(identity, reason).phase == Phase.HALTED
    assert RecoveryLedger(tmp_path / 'state.db').read(identity).halted_reason == reason
    with pytest.raises(RecoveryError, match='Stale checkpoint'):
        ledger.advance(identity, (Phase.PLANNED, Phase.WORKER_ADMITTED), 'a' * 64)


def test_recovery_rejects_skipping_phases(tmp_path: Path, identity: RecoveryIdentity) -> None:
    ledger = RecoveryLedger(tmp_path / 'state.db')
    ledger.initialize(identity)
    with pytest.raises(RecoveryError, match='skip'):
        ledger.advance(identity, (Phase.PLANNED, Phase.REVIEWER_ADMITTED), 'a' * 64)


def test_pending_decision_requires_unapproved_identity(
    tmp_path: Path, identity: RecoveryIdentity
) -> None:
    path = tmp_path / 'human-decision-request.json'
    record = {
        'task_id': 'task',
        'worker_submission_id': 'worker',
        'reviewer_submission_id': 'reviewer',
        'human_decision': 'pending',
        'approved': False,
    }
    path.write_text(json.dumps(record))
    assert len(inspect_pending_decision(path, identity)) == hashlib.sha256().digest_size * 2
    record['approved'] = True
    path.write_text(json.dumps(record))
    with pytest.raises(RecoveryError, match='not pending'):
        inspect_pending_decision(path, identity)


def test_pending_checkpoint_never_approves(tmp_path: Path, identity: RecoveryIdentity) -> None:
    ledger = RecoveryLedger(tmp_path / 'state.db')
    ledger.initialize(identity)
    for old, new in zip(
        (Phase.PLANNED, Phase.WORKER_ADMITTED, Phase.WORKER_AUDITED),
        (Phase.WORKER_ADMITTED, Phase.WORKER_AUDITED, Phase.REVIEWER_ADMITTED),
        strict=True,
    ):
        ledger.advance(identity, (old, new), 'a' * 64)
    path = tmp_path / 'human-decision-request.json'
    path.write_text(
        json.dumps({
            'task_id': identity.task_id,
            'worker_submission_id': identity.worker_submission_id,
            'reviewer_submission_id': identity.reviewer_submission_id,
            'human_decision': 'pending',
            'approved': False,
        })
    )
    assert checkpoint_pending_human(ledger, identity, path).phase == Phase.PENDING_HUMAN
    assert json.loads(path.read_text())['approved'] is False
    with pytest.raises(RecoveryError, match='Stale checkpoint'):
        checkpoint_pending_human(ledger, identity, path)
