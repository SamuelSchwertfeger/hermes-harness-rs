"""Strict admission of model- or Hermes-produced multi-agent plans.

No provider calls or task execution occur here. Untrusted model text is admitted
only after size, schema, field, role and canonical DAG validation.
"""

import json
from typing import Any

from hermes_harness.multiagent.runtime import Node, Plan, WorkflowError, ordered_nodes

MAX_PLAN_BYTES = 64 * 1024
MAX_OBJECTIVE_CHARS = 4096
MAX_IDENTITY_CHARS = 128
PLAN_KEYS = frozenset({'id', 'objective', 'nodes', 'primary_identity'})
NODE_KEYS = frozenset({'id', 'objective', 'owned_paths', 'dependencies', 'role'})


def _strings(value: object, field: str) -> tuple[str, ...]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        msg = f'{field} must be an array of strings'
        raise WorkflowError(msg)
    return tuple(value)


def _bounded_text(value: object, field: str, limit: int) -> str:
    if not isinstance(value, str) or not value.strip() or len(value) > limit:
        msg = f'Invalid {field}'
        raise WorkflowError(msg)
    return value


def _node(value: object) -> Node:
    if not isinstance(value, dict) or set(value) - NODE_KEYS:
        msg = 'Invalid node object or unknown fields'
        raise WorkflowError(msg)
    node_id = _bounded_text(value.get('id'), 'node ID', MAX_IDENTITY_CHARS)
    objective = _bounded_text(value.get('objective'), 'node objective', MAX_OBJECTIVE_CHARS)
    paths = _strings(value.get('owned_paths'), 'owned_paths')
    dependencies = _strings(value.get('dependencies', []), 'dependencies')
    role = _bounded_text(value.get('role', 'coding'), 'role', MAX_IDENTITY_CHARS)
    return Node(node_id, objective, paths, dependencies, role)


def parse_plan_document(value: object) -> Plan:
    """Reject unknown fields, ambiguous types and invalid DAGs before execution."""
    if not isinstance(value, dict) or set(value) - PLAN_KEYS:
        msg = 'Invalid plan object or unknown fields'
        raise WorkflowError(msg)
    plan_id = _bounded_text(value.get('id'), 'plan ID', MAX_IDENTITY_CHARS)
    objective = _bounded_text(value.get('objective'), 'plan objective', MAX_OBJECTIVE_CHARS)
    identity = _bounded_text(
        value.get('primary_identity', 'hermes-primary'), 'primary identity', MAX_IDENTITY_CHARS
    )
    rows = value.get('nodes')
    if not isinstance(rows, list):
        msg = 'Plan nodes must be an array'
        raise WorkflowError(msg)
    plan = Plan(plan_id, objective, tuple(_node(row) for row in rows), identity)
    ordered_nodes(plan)
    return plan


def parse_plan_json(payload: str) -> Plan:
    """Parse one JSON document; refuse oversized or duplicate-key model output."""
    if len(payload.encode('utf-8')) > MAX_PLAN_BYTES:
        msg = 'Plan exceeds input byte limit'
        raise WorkflowError(msg)

    def unique_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                msg = 'Duplicate JSON key'
                raise WorkflowError(msg)
            result[key] = value
        return result

    try:
        value: object = json.loads(payload, object_pairs_hook=unique_pairs)
    except (ValueError, UnicodeError) as exc:
        msg = 'Malformed plan JSON'
        raise WorkflowError(msg) from exc
    return parse_plan_document(value)
