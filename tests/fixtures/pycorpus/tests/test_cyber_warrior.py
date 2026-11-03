"""Tests for scoped Cyber Warrior dispatch authorization."""

from typing import TYPE_CHECKING

import pytest

from hermes_harness.multiagent.cyber import (
    CyberAuthorizationError,
    CyberGrant,
    validate_cyber_node,
)
from hermes_harness.multiagent.runtime import Node, Plan, WorkflowError, ordered_nodes

if TYPE_CHECKING:
    from pathlib import Path


def test_missing_grant_fails_closed(tmp_path: Path) -> None:
    with pytest.raises(CyberAuthorizationError, match='explicit operator grant'):
        validate_cyber_node('audit', ('src/a.py',), tmp_path, None)


def test_authorization_is_scoped(tmp_path: Path) -> None:
    grant = CyberGrant(tmp_path, ('audit',), ('src/a.py',), 'operator')
    validate_cyber_node('audit', ('src/a.py',), tmp_path, grant)
    with pytest.raises(CyberAuthorizationError, match='task not authorized'):
        validate_cyber_node('other', ('src/a.py',), tmp_path, grant)
    with pytest.raises(CyberAuthorizationError, match='exceeds grant'):
        validate_cyber_node('audit', ('src/b.py',), tmp_path, grant)
    with pytest.raises(CyberAuthorizationError, match='repository mismatch'):
        validate_cyber_node('audit', ('src/a.py',), tmp_path.parent, grant)


def test_roles_are_validated() -> None:
    plan = Plan('security', 'review', (Node('audit', 'review code', ('src/a.py',), role='root'),))
    with pytest.raises(WorkflowError, match='Unknown worker role'):
        ordered_nodes(plan)
    cyber = Plan(
        'security',
        'review',
        (Node('audit', 'review code', ('src/a.py',), role='cyber-warrior'),),
    )
    assert ordered_nodes(cyber)[0].role == 'cyber-warrior'
