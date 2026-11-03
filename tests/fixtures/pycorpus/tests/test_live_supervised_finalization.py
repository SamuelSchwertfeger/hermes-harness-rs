"""Offline orchestration tests; no provider, network or process execution."""

from typing import TYPE_CHECKING
from unittest.mock import AsyncMock, Mock, patch

import pytest

from hermes_harness.supervised.live_supervised_finalization import (
    FinalizationBlockedError,
    FinalizationInputs,
    finalize_existing_roles_once,
)
from hermes_harness.supervised.live_supervised_recovery import (
    Phase,
    RecoveryIdentity,
    RecoveryLedger,
    evidence_digest,
)

if TYPE_CHECKING:
    from pathlib import Path


@pytest.fixture
def context(tmp_path: Path) -> FinalizationInputs:
    identity = RecoveryIdentity(
        task_id='task', worker_submission_id='worker', reviewer_submission_id='reviewer'
    )
    plan = Mock(task_id='task')
    task = Mock()
    task.brief.task_id = 'task'
    handoff = Mock(task_id='task', worker_submission_id='worker')
    admissions = Mock()
    receipts = Mock(store=admissions)
    return FinalizationInputs(
        plan=plan,
        task=task,
        handoff=handoff,
        admissions=admissions,
        receipts=receipts,
        recovery=RecoveryLedger(tmp_path / 'recovery.db'),
        identity=identity,
        output_dir=tmp_path,
    )


@pytest.mark.asyncio
async def test_complete_offline_finalization(context: FinalizationInputs) -> None:
    with (
        patch('hermes_harness.supervised.live_supervised_finalization.verify_reviewer_handoff'),
        patch(
            'hermes_harness.supervised.live_supervised_finalization._advance_evidence'
        ) as advance,
        patch(
            'hermes_harness.supervised.live_supervised_finalization.reconcile_supervised_roles_once',
            new_callable=AsyncMock,
        ) as reconcile,
        patch(
            'hermes_harness.supervised.live_supervised_finalization.request_human_decision_once'
        ) as write,
        patch(
            'hermes_harness.supervised.live_supervised_finalization.checkpoint_pending_human'
        ) as pending,
    ):
        advance.side_effect = [
            Mock(phase=Phase.WORKER_ADMITTED),
            Mock(phase=Phase.WORKER_AUDITED),
            Mock(phase=Phase.REVIEWER_ADMITTED),
        ]
        reconcile.return_value = Mock(worker=Mock(), reviewer=Mock())
        pending.return_value = Mock(phase=Phase.PENDING_HUMAN)
        result = await finalize_existing_roles_once(context)
    assert result.phase == Phase.PENDING_HUMAN
    expected_transitions = 3
    assert advance.call_count == expected_transitions
    reconcile.assert_awaited_once()
    write.assert_called_once()
    pending.assert_called_once()


@pytest.mark.asyncio
async def test_halted_checkpoint_does_not_reconcile(context: FinalizationInputs) -> None:
    context.recovery.initialize(context.identity)
    context.recovery.halt(context.identity, 'operator denied')
    with (
        patch('hermes_harness.supervised.live_supervised_finalization.verify_reviewer_handoff'),
        patch(
            'hermes_harness.supervised.live_supervised_finalization.reconcile_supervised_roles_once',
            new_callable=AsyncMock,
        ) as reconcile,
        pytest.raises(FinalizationBlockedError, match='halted'),
    ):
        await finalize_existing_roles_once(context)
    reconcile.assert_not_awaited()


@pytest.mark.asyncio
async def test_missing_pending_artifact_cannot_be_regenerated(context: FinalizationInputs) -> None:
    context.recovery.initialize(context.identity)
    transitions = (
        (Phase.PLANNED, Phase.WORKER_ADMITTED),
        (Phase.WORKER_ADMITTED, Phase.WORKER_AUDITED),
        (Phase.WORKER_AUDITED, Phase.REVIEWER_ADMITTED),
        (Phase.REVIEWER_ADMITTED, Phase.PENDING_HUMAN),
    )
    for transition in transitions:
        context.recovery.advance(context.identity, transition, evidence_digest(transition))
    with (
        patch('hermes_harness.supervised.live_supervised_finalization.verify_reviewer_handoff'),
        patch(
            'hermes_harness.supervised.live_supervised_finalization.request_human_decision_once'
        ) as write,
        pytest.raises(FinalizationBlockedError, match='missing'),
    ):
        await finalize_existing_roles_once(context)
    write.assert_not_called()


@pytest.mark.asyncio
async def test_wrong_ledger_blocks_before_gateway(context: FinalizationInputs) -> None:
    context.receipts.store = Mock()
    with (
        patch(
            'hermes_harness.supervised.live_supervised_finalization.reconcile_supervised_roles_once',
            new_callable=AsyncMock,
        ) as reconcile,
        pytest.raises(FinalizationBlockedError, match='authoritative'),
    ):
        await finalize_existing_roles_once(context)
    reconcile.assert_not_awaited()


@pytest.mark.asyncio
async def test_existing_decision_never_writes_twice(context: FinalizationInputs) -> None:
    path = context.output_dir / 'human-decision-request.json'
    path.write_text('evidence')
    with (
        patch('hermes_harness.supervised.live_supervised_finalization.verify_reviewer_handoff'),
        patch(
            'hermes_harness.supervised.live_supervised_finalization.inspect_pending_decision',
            return_value='a' * 64,
        ),
        patch(
            'hermes_harness.supervised.live_supervised_finalization.request_human_decision_once'
        ) as write,
        pytest.raises(FinalizationBlockedError, match='conflicts'),
    ):
        await finalize_existing_roles_once(context)
    write.assert_not_called()


@pytest.mark.asyncio
async def test_pending_request_recovered_without_rewrite(context: FinalizationInputs) -> None:
    context.recovery.initialize(context.identity)
    for transition in (
        (Phase.PLANNED, Phase.WORKER_ADMITTED),
        (Phase.WORKER_ADMITTED, Phase.WORKER_AUDITED),
        (Phase.WORKER_AUDITED, Phase.REVIEWER_ADMITTED),
    ):
        context.recovery.advance(context.identity, transition, evidence_digest(transition))
    path = context.output_dir / 'human-decision-request.json'
    path.write_text('existing decision')
    with (
        patch('hermes_harness.supervised.live_supervised_finalization.verify_reviewer_handoff'),
        patch(
            'hermes_harness.supervised.live_supervised_finalization.inspect_pending_decision',
            return_value='a' * 64,
        ),
        patch(
            'hermes_harness.supervised.live_supervised_finalization.request_human_decision_once'
        ) as write,
        patch(
            'hermes_harness.supervised.live_supervised_finalization.checkpoint_pending_human'
        ) as pending,
    ):
        pending.return_value = Mock(phase=Phase.PENDING_HUMAN)
        result = await finalize_existing_roles_once(context)
    assert result.phase == Phase.PENDING_HUMAN
    write.assert_not_called()
    pending.assert_called_once()
    assert path.read_text() == 'existing decision'
