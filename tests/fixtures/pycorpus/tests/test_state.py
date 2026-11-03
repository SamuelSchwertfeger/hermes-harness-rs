from pathlib import Path  # noqa: TC003 - runtime annotation on Python 3.13

import pytest

from hermes_harness.core.state import Stage, create, load, save


def test_transitions_and_persistence(tmp_path: Path) -> None:
    ticket = create(tmp_path, 'test-001', 'Bounded review')
    with pytest.raises(ValueError, match='Transition forbidden'):
        ticket.advance(Stage.IMPLEMENT)
    with pytest.raises(ValueError, match='facts'):
        ticket.advance(Stage.FACTS)
    ticket.evidence['facts'] = 'facts.md reviewed'
    ticket.advance(Stage.FACTS)
    ticket.advance(Stage.TICKET)
    ticket.evidence['plan'] = 'plan.md'
    ticket.advance(Stage.PLAN)
    with pytest.raises(PermissionError, match='plan'):
        ticket.advance(Stage.IMPLEMENT)
    ticket.approvals['plan'] = True
    ticket.advance(Stage.IMPLEMENT)
    save(tmp_path, ticket)
    assert load(tmp_path, 'test-001').stage == Stage.IMPLEMENT


def test_slug_guard(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match='lower-case slug'):
        create(tmp_path, '../escape', 'x')
