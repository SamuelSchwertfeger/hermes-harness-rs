"""Durable multi-task orchestration over canonical audited offline delegation.

All injected agents are trusted deterministic doubles. This is NOT an OS sandbox,
and no provider, gateway, network, Git merge, or external tool is invoked by agents.
"""

import hashlib
import json
import os
import re
from dataclasses import asdict, dataclass
from typing import TYPE_CHECKING, Protocol

from hermes_harness.core.delegation import Brief, Finding
from hermes_harness.git.worktree_delegation import TaskWorkspace
from hermes_harness.multiagent.cyber import validate_cyber_node
from hermes_harness.supervised.offline_orchestrator import OfflineAgents, run_offline_task

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.core.delegation import OfflineAgent
    from hermes_harness.multiagent.cyber import CyberGrant

_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,63}\Z')
MAX_NODES = 32
MAX_PATHS = 32
MAX_TASK_ID_LENGTH = 64


class WorkflowError(RuntimeError):
    """Invalid plan or fenced workflow; operator inspection required."""


@dataclass(frozen=True, slots=True)
class Node:
    """Bounded task node with exclusive ownership and dependencies."""

    id: str
    objective: str
    owned_paths: tuple[str, ...]
    dependencies: tuple[str, ...] = ()
    role: str = 'coding'


@dataclass(frozen=True, slots=True)
class Plan:
    """Primary coordinator output, validated before any worktree operation."""

    id: str
    objective: str
    nodes: tuple[Node, ...]
    primary_identity: str = 'offline-primary'


class Primary(Protocol):
    """Trusted offline planner contract; never given execution capabilities."""

    def plan(self, request: str) -> Plan: ...


class WorkerFactory(Protocol):
    """Create an independent offline double for a bound task workspace."""

    def worker(self, node: Node, workspace: Path) -> OfflineAgent: ...

    def reviewer(self, node: Node) -> OfflineAgent: ...


def _validate_path(path: str, owners: list[str]) -> None:
    parts = path.split('/')
    unsafe = path.startswith('/') or '\\' in path or '\x00' in path
    unsafe = unsafe or any(part in ('', '.', '..') for part in parts)
    unsafe = unsafe or parts[0] == '.git'
    if unsafe:
        msg = 'Unsafe ownership path'
        raise WorkflowError(msg)
    if any(
        path == other or path.startswith(f'{other}/') or other.startswith(f'{path}/')
        for other in owners
    ):
        msg = 'Overlapping ownership'
        raise WorkflowError(msg)
    owners.append(path)


def _validate_node(node: Node, by_id: dict[str, Node], owners: list[str]) -> None:
    if node.role not in ('coding', 'cyber-warrior'):
        msg = 'Unknown worker role'
        raise WorkflowError(msg)
    if not _ID.fullmatch(node.id) or not node.objective.strip():
        msg = 'Invalid node'
        raise WorkflowError(msg)
    if not node.owned_paths or len(node.owned_paths) > MAX_PATHS:
        msg = 'Invalid ownership cardinality'
        raise WorkflowError(msg)
    for path in node.owned_paths:
        _validate_path(path, owners)
    if node.id in node.dependencies or len(set(node.dependencies)) != len(node.dependencies):
        msg = 'Invalid dependencies'
        raise WorkflowError(msg)
    if any(dependency not in by_id for dependency in node.dependencies):
        msg = 'Missing dependency'
        raise WorkflowError(msg)


def _topological_order(nodes: tuple[Node, ...]) -> tuple[Node, ...]:
    done: set[str] = set()
    result: list[Node] = []
    while len(result) < len(nodes):
        ready = sorted(
            (node for node in nodes if node.id not in done and set(node.dependencies) <= done),
            key=lambda node: node.id,
        )
        if not ready:
            msg = 'Cyclic dependencies'
            raise WorkflowError(msg)
        result.append(ready[0])
        done.add(ready[0].id)
    return tuple(result)


@dataclass(frozen=True, slots=True)
class ExecutionConfig:
    """Offline execution dependencies supplied by the operator."""

    repo: Path
    factory: WorkerFactory
    cyber_grant: CyberGrant | None = None


@dataclass(frozen=True, slots=True)
class _DispatchContext:
    """Shared immutable fields for a single run's dispatches."""

    directory: Path
    plan_id: str
    execution: ExecutionConfig


def ordered_nodes(plan: Plan) -> tuple[Node, ...]:
    """Validate DAG, IDs and disjoint file ownership, then stable topological sort."""
    if not _ID.fullmatch(plan.id) or not plan.objective.strip():
        msg = 'Invalid plan identity or objective'
        raise WorkflowError(msg)
    if not 1 <= len(plan.nodes) <= MAX_NODES:
        msg = 'Plan must contain between 1 and 32 nodes'
        raise WorkflowError(msg)
    by_id = {node.id: node for node in plan.nodes}
    if len(by_id) != len(plan.nodes):
        msg = 'Duplicate node ID'
        raise WorkflowError(msg)
    owners: list[str] = []
    for node in plan.nodes:
        _validate_node(node, by_id, owners)
    return _topological_order(plan.nodes)


def _write_once(path: Path, value: object) -> None:
    payload = json.dumps(value, sort_keys=True, separators=(',', ':')).encode() + b'\n'
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, 'O_NOFOLLOW'):
        flags |= os.O_NOFOLLOW
    with os.fdopen(os.open(path, flags, 0o600), 'wb') as stream:
        stream.write(payload)
        stream.flush()
        os.fsync(stream.fileno())


def _record(path: Path) -> dict[str, object]:
    if path.is_symlink():
        msg = 'Symlink record rejected'
        raise WorkflowError(msg)
    value: object = json.loads(path.read_bytes())
    if not isinstance(value, dict):
        msg = 'Invalid record'
        raise WorkflowError(msg)
    return value


def _task_status(directory: Path, name: str) -> dict[str, object]:
    receipt = directory / f'{name}.receipt.json'
    fence = directory / f'{name}.dispatch-fence.json'
    if receipt.is_file():
        record = _record(receipt)
        status = 'completed' if record.get('state') == 'awaiting_approval' else 'failed'
    else:
        status = 'unknown' if fence.exists() else 'pending'
    return {'node': name, 'state': status}


def inspect(root: Path, run_id: str) -> dict[str, object]:
    """Read-only, restart-safe inspection; never redispatch uncertain work."""
    if not _ID.fullmatch(run_id):
        msg = 'Invalid run ID'
        raise WorkflowError(msg)
    directory = root / run_id
    plan = _record(directory / 'plan.json')
    nodes = plan.get('nodes')
    if not isinstance(nodes, list):
        msg = 'Invalid persisted plan'
        raise WorkflowError(msg)
    results = _persisted_tasks(directory, nodes)
    decision_path = directory / 'decision.json'
    decision_record = _record(decision_path) if decision_path.exists() else None
    state = 'pending_human_decision' if decision_record is not None else 'fenced_or_incomplete'
    return {
        'run_id': run_id,
        'tasks': results,
        'state': state,
        'decision': decision_record,
        'task_evidence': str(directory / 'tasks'),
    }


def _persisted_tasks(directory: Path, nodes: list[object]) -> list[dict[str, object]]:
    results: list[dict[str, object]] = []
    for node in nodes:
        if not isinstance(node, dict) or not isinstance(node.get('id'), str):
            msg = 'Invalid persisted plan'
            raise WorkflowError(msg)
        results.append(_task_status(directory, node['id']))
    return results


def _dispatch_node(context: _DispatchContext, node: Node) -> None:
    directory = context.directory
    task_id = f'{context.plan_id}-{node.id}'
    if len(task_id) > MAX_TASK_ID_LENGTH:
        msg = 'Combined task ID too long'
        raise WorkflowError(msg)
    brief = Brief(
        task_id, node.objective, node.owned_paths, f'worker-{node.id}', f'reviewer-{node.id}'
    )
    workspace = directory / 'workspaces' / node.id
    task = TaskWorkspace(directory / 'tasks', workspace, brief)
    _write_once(
        directory / f'{node.id}.dispatch-fence.json',
        {
            'task_id': task_id,
            'role': node.role,
            'objective_sha256': hashlib.sha256(node.objective.encode()).hexdigest(),
            'state': 'uncertain_until_verified',
        },
    )
    worker = context.execution.factory.worker(node, workspace)
    reviewer = context.execution.factory.reviewer(node)
    if worker is reviewer:
        msg = 'Worker and reviewer must be independent'
        raise WorkflowError(msg)
    result = run_offline_task(task, context.execution.repo, OfflineAgents(worker, reviewer))
    _write_once(
        directory / f'{node.id}.receipt.json',
        {
            'task_id': task_id,
            'state': result.state.value,
            'reviewer_id': brief.reviewer_id,
            'worker_sha256': (
                hashlib.sha256(result.worker.text.encode()).hexdigest() if result.worker else None
            ),
            'review_sha256': (
                hashlib.sha256(result.reviewer.text.encode()).hexdigest()
                if result.reviewer
                else None
            ),
            'approval_request': str(result.approval_request),
        },
    )


def execute(root: Path, plan: Plan, execution: ExecutionConfig) -> dict[str, object]:  # noqa: C901 - preserve audited boundary and established call contract
    """Run one fresh serial offline plan, fencing every uncertain dispatch."""
    ordered = ordered_nodes(plan)
    for node in ordered:
        if node.role == 'cyber-warrior':
            validate_cyber_node(node.id, node.owned_paths, execution.repo, execution.cyber_grant)
    if not root.is_dir() or root.is_symlink():
        msg = 'State root must be a real existing directory'
        raise WorkflowError(msg)
    directory = root / plan.id
    directory.mkdir(mode=0o700, exist_ok=False)
    for folder in ('tasks', 'workspaces'):
        (directory / folder).mkdir(mode=0o700)
    _write_once(directory / 'plan.json', asdict(plan))
    context = _DispatchContext(directory, plan.id, execution)
    for node in ordered:
        for dependency in node.dependencies:
            prior = _task_status(directory, dependency)
            if prior['state'] != 'completed':
                msg = 'Cannot dispatch node after unsuccessful dependency'
                raise WorkflowError(msg)
        _dispatch_node(context, node)
        current = _task_status(directory, node.id)
        if current['state'] != 'completed':
            msg = 'Node outcome is not eligible for downstream dispatch'
            raise WorkflowError(msg)
    _write_once(
        directory / 'decision.json',
        {'state': 'pending', 'approved': False, 'integrated': False, 'run_id': plan.id},
    )
    return inspect(root, plan.id)


def decide(root: Path, run_id: str, action: str) -> dict[str, object]:
    """Write an immutable operator decision, without applying changes."""
    if action not in ('approve', 'reject', 'request-changes'):
        msg = 'Invalid human decision'
        raise WorkflowError(msg)
    summary = inspect(root, run_id)
    tasks = summary['tasks']
    if (
        summary['state'] != 'pending_human_decision'
        or not isinstance(tasks, list)
        or any(not isinstance(row, dict) or row.get('state') != 'completed' for row in tasks)
    ):
        msg = 'Cannot decide on incomplete or uncertain run'
        raise WorkflowError(msg)
    _write_once(
        root / run_id / 'human-decision.json',
        {'decision': action, 'does_not_authorize_merge_or_push': True},
    )
    return {'run_id': run_id, 'decision': action, 'applied': False}


@dataclass(frozen=True, slots=True)
class DemoWorker:
    """Trusted offline test double, not a sandboxed agent."""

    workspace: Path
    target: str
    node_id: str

    def run(self, brief: Brief, prior: Finding | None) -> Finding:
        if prior is not None:
            msg = 'Worker received reviewer context'
            raise WorkflowError(msg)
        path = self.workspace / self.target
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f'offline evidence for {self.node_id}\n', encoding='utf-8')
        return Finding(brief.worker_id, f'Created {self.target} in disposable worktree')


@dataclass(frozen=True, slots=True)
class DemoReviewer:
    def run(self, brief: Brief, prior: Finding | None) -> Finding:
        if prior is None:
            msg = 'Reviewer missing worker evidence'
            raise WorkflowError(msg)
        return Finding(brief.reviewer_id, 'Offline reviewer received audited worker evidence')


class DemoFactory:
    def worker(self, node: Node, workspace: Path) -> OfflineAgent:
        return DemoWorker(workspace, node.owned_paths[0], node.id)

    def reviewer(self, node: Node) -> OfflineAgent:
        _ = node
        return DemoReviewer()
