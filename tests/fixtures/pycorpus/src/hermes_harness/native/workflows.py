"""Hermes-native CLI skills and independently authorized local-model adapters."""

import json
import re
import shutil
import subprocess
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.multiagent.apply_edits import apply_new_files
from hermes_harness.multiagent.local_inference import (
    propose_edits,
    propose_plan,
)
from hermes_harness.multiagent.model_pipeline import generate_and_review
from hermes_harness.multiagent.ollama_discovery import ollama_models
from hermes_harness.multiagent.plan_io import parse_plan_json
from hermes_harness.multiagent.replacement_pipeline import generate_reviewed_replacements
from hermes_harness.multiagent.routing import OllamaRouting, qualify_models
from hermes_harness.multiagent.runtime import (
    DemoFactory,
    ExecutionConfig,
    Node,
    Plan,
    WorkflowError,
    execute,
    inspect,
    ordered_nodes,
)

if TYPE_CHECKING:
    from hermes_harness.multiagent.local_inference import (
        LocalEndpoint,
        Transport,
    )
    from hermes_harness.multiagent.model_edits import EditProposal

_SAMPLE_LIMIT = 40
_MAX_PLAN_BYTES = 65_536
_RUN_ID = re.compile(r'[a-z0-9][a-z0-9-]{0,63}\Z')


def repository(path: Path) -> Path:
    """Require an existing, real Git checkout (never create or clone)."""
    if path.is_symlink() or not path.is_dir():
        msg = 'Repository must be a real directory'
        raise WorkflowError(msg)
    root = path.resolve(strict=True)
    git = shutil.which('git')
    if git is None:
        msg = 'Git is not installed'
        raise WorkflowError(msg)
    result = subprocess.run(  # noqa: S603
        [git, '-C', str(root), 'rev-parse', '--show-toplevel'],
        capture_output=True,
        text=True,
        timeout=10,
        check=False,
        shell=False,
    )
    if result.returncode or Path(result.stdout.strip()).resolve() != root:
        msg = 'Path must be the root of a Git checkout'
        raise WorkflowError(msg)
    return root


def _git(root: Path, *args: str) -> str:
    git = shutil.which('git')
    if git is None:
        msg = 'Git is not installed'
        raise WorkflowError(msg)
    result = subprocess.run(  # noqa: S603
        [git, '-C', str(root), *args],
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
        shell=False,
    )
    if result.returncode:
        msg = 'Git inspection failed'
        raise WorkflowError(msg)
    return result.stdout


def investigate(repo: Path) -> dict[str, object]:
    """Return read-only bounded evidence, without reading source or secrets."""
    root = repository(repo)
    tracked = _git(root, 'ls-files', '--').splitlines()
    status = _git(root, 'status', '--short', '--untracked-files=no').splitlines()
    return {
        'action': 'lets-investigate',
        'mode': 'read-only',
        'repo': str(root),
        'head': _git(root, 'rev-parse', 'HEAD').strip(),
        'branch': _git(root, 'branch', '--show-current').strip(),
        'tracked_count': len(tracked),
        'tracked_sample': tracked[:_SAMPLE_LIMIT],
        'tracked_truncated': len(tracked) > _SAMPLE_LIMIT,
        'changes': status[:_SAMPLE_LIMIT],
        'changes_truncated': len(status) > _SAMPLE_LIMIT,
        'note': 'Metadata-only investigation; no source files read or modified',
    }


def parse_plan(data: object) -> Plan:  # noqa: C901, PLR0915, PLR0914 - preserve audited boundary and established call contract
    """Reject invalid plan shapes before calling canonical DAG validator."""
    if not isinstance(data, dict):
        msg = 'Plan must be a JSON object'
        raise WorkflowError(msg)
    plan_id = data.get('id')
    objective = data.get('objective')
    rows = data.get('nodes')
    if not isinstance(plan_id, str) or not isinstance(objective, str) or not isinstance(rows, list):
        msg = 'Plan needs id, objective and nodes'
        raise WorkflowError(msg)
    nodes: list[Node] = []
    for row in rows:
        if not isinstance(row, dict):
            msg = 'Each node must be a JSON object'
            raise WorkflowError(msg)
        node_id, node_objective = row.get('id'), row.get('objective')
        paths, deps = row.get('owned_paths'), row.get('dependencies', [])
        if not isinstance(node_id, str) or not isinstance(node_objective, str):
            msg = 'Invalid node fields'
            raise WorkflowError(msg)
        if not isinstance(paths, list) or not all(isinstance(p, str) for p in paths):
            msg = 'Invalid node fields'
            raise WorkflowError(msg)
        if not isinstance(deps, list) or not all(isinstance(d, str) for d in deps):
            msg = 'Invalid node fields'
            raise WorkflowError(msg)
        nodes.append(Node(node_id, node_objective, tuple(paths), tuple(deps)))
    identity = data.get('primary_identity', 'offline-primary')
    if not isinstance(identity, str):
        msg = 'Invalid primary identity'
        raise WorkflowError(msg)
    plan = Plan(plan_id, objective, tuple(nodes), identity)
    ordered_nodes(plan)
    return plan


def _game_plan_file(file: Path) -> dict[str, object]:
    """Validate an operator/primary supplied JSON plan; do not fabricate a model plan."""
    if file.is_symlink() or not file.is_file() or file.stat().st_size > _MAX_PLAN_BYTES:
        msg = 'Plan must be a real JSON file of at most 64KiB'
        raise WorkflowError(msg)
    try:
        plan = parse_plan(json.loads(file.read_text(encoding='utf-8')))
    except (ValueError, UnicodeError) as exc:
        msg = 'Invalid plan JSON'
        raise WorkflowError(msg) from exc
    return {
        'action': 'game-plan',
        'mode': 'offline-validated',
        'plan': asdict(plan),
        'execution_order': [node.id for node in ordered_nodes(plan)],
        'note': 'Plan supplied externally; no primary model invoked',
    }


def assemble(repo: Path, state: Path, file: Path) -> dict[str, object]:
    """Execute canonical deterministic doubles; no model/network/merge."""
    root = repository(repo)
    if file.is_symlink() or not file.is_file() or file.stat().st_size > _MAX_PLAN_BYTES:
        msg = 'Invalid plan file'
        raise WorkflowError(msg)
    plan = parse_plan(json.loads(file.read_text(encoding='utf-8')))
    if state.is_symlink() or (state.exists() and not state.is_dir()):
        msg = 'Invalid state directory'
        raise WorkflowError(msg)
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    result = execute(state.resolve(), plan, ExecutionConfig(root, DemoFactory()))
    return {
        'action': 'agents-assemble',
        'mode': 'offline-simulated',
        'simulated_agents': True,
        'result': result,
        'note': 'Deterministic doubles only; no live model, integration, merge or push',
    }


def inspect_run(state: Path, run_id: str) -> dict[str, object]:
    if not _RUN_ID.fullmatch(run_id):
        msg = 'Invalid run ID'
        raise WorkflowError(msg)
    return {'action': 'inspect', 'result': inspect(state, run_id)}


@dataclass(frozen=True, slots=True)
class NativeContext:
    """Trusted adapter context: originating session, repository and state root."""

    session_id: str
    repository: Path
    state_root: Path


MAX_SESSION_ID_LENGTH = 128


def _validate_context(context: NativeContext) -> None:
    if not context.session_id.strip() or len(context.session_id) > MAX_SESSION_ID_LENGTH:
        msg = 'Invalid originating Hermes session'
        raise WorkflowError(msg)
    if context.repository.is_symlink() or not context.repository.is_dir():
        msg = 'Repository must be an existing real directory'
        raise WorkflowError(msg)
    if context.state_root.is_symlink() or not context.state_root.is_dir():
        msg = 'State root must be an existing real directory'
        raise WorkflowError(msg)


def validate_native_plan(context: NativeContext, plan: Plan) -> dict[str, object]:
    """Validate a canonical bounded plan with no dispatch or mutations."""
    _validate_context(context)
    ordered = ordered_nodes(plan)
    return {
        'session_id': context.session_id,
        'plan_id': plan.id,
        'ordered_nodes': [node.id for node in ordered],
        'roles': {node.id: node.role for node in ordered},
        'mode': 'validation-only',
    }


def agents_assemble_offline(
    context: NativeContext, plan: Plan, *, authorized: bool = False
) -> dict[str, object]:
    """Require explicit offline authorization; persist auditable simulated execution.

    This entry point refuses Cyber Warrior dispatch: trusted CyberGrant transport
    and native Hermes caller permissions are not yet host-qualified.
    """
    _validate_context(context)
    if not authorized:
        msg = 'Explicit offline authorization is required'
        raise WorkflowError(msg)
    ordered = ordered_nodes(plan)
    if any(node.role == 'cyber-warrior' for node in ordered):
        msg = 'Cyber Warrior native grant transport is not qualified'
        raise WorkflowError(msg)
    result = execute(context.state_root, plan, ExecutionConfig(context.repository, DemoFactory()))
    return {
        'session_id': context.session_id,
        'mode': 'simulated-offline',
        'real_models': False,
        'result': result,
    }


def what_we_know(context: NativeContext, run_id: str) -> dict[str, object]:
    """Read persisted evidence without retrying, recovering, or approving work."""
    _validate_context(context)
    return {
        'session_id': context.session_id,
        'mode': 'read-only',
        'result': inspect(context.state_root, run_id),
    }


def admit_plan_json(context: NativeContext, payload: str) -> dict[str, object]:
    """Validate Hermes primary model output before any authorization or dispatch."""
    return validate_native_plan(context, parse_plan_json(payload))


def propose_local_plan(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    context: NativeContext,
    endpoint: LocalEndpoint,
    task: str,
    transport: Transport,
    *,
    authorized: bool = False,
) -> dict[str, object]:
    """Opt-in inference-only coordinator; no tools, worktrees, or dispatch."""
    _validate_context(context)
    if not authorized:
        msg = 'Explicit local inference authorization required'
        raise WorkflowError(msg)
    plan = propose_plan(endpoint, task, transport)
    result = validate_native_plan(context, plan)
    result['mode'] = 'local-model-plan-validation'
    result['model'] = endpoint.model
    result['real_inference'] = True
    result['worker_execution'] = False
    return result


def propose_local_edits(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    context: NativeContext,
    endpoint: LocalEndpoint,
    node: Node,
    transport: Transport,
    *,
    authorized: bool = False,
) -> dict[str, object]:
    """Return scoped proposal digests, never edit files or execute generated text."""
    _validate_context(context)
    if not authorized:
        msg = 'Explicit local inference authorization required'
        raise WorkflowError(msg)
    if node.role == 'cyber-warrior':
        msg = 'Cyber Warrior trusted grant transport is unqualified'
        raise WorkflowError(msg)
    if node.role != 'coding':
        msg = 'Unsupported local model worker role'
        raise WorkflowError(msg)
    result = propose_edits(endpoint, node.id, node.objective, node.owned_paths, transport)
    return {
        'session_id': context.session_id,
        'mode': 'local-model-proposal-only',
        'task_id': result.task_id,
        'files': [{'path': file.path, 'sha256': file.sha256} for file in result.files],
        'applied': False,
        'reviewed': False,
    }


def apply_local_new_file_proposal(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    context: NativeContext,
    workspace: Path,
    proposal: EditProposal,
    owned_paths: tuple[str, ...],
    baseline: str,
    *,
    authorized: bool = False,
) -> dict[str, object]:
    """Apply an already admitted proposal to a caller-selected disposable worktree.

    This is not live autonomous worker dispatch or a substitute for OS isolation.
    """
    _validate_context(context)
    result = apply_new_files(workspace, proposal, owned_paths, baseline, authorized=authorized)
    return {
        'session_id': context.session_id,
        'mode': 'authorized-disposable-new-files',
        'task_id': result.task_id,
        'changed_paths': result.audit.changed_paths,
        'baseline': result.audit.baseline,
        'head': result.audit.head,
        'merged': False,
        'verified_by_tests': False,
    }


def discover_local_ollama_models(context: NativeContext) -> dict[str, object]:
    """Read-only model listing from this host's configured loopback Ollama."""
    _validate_context(context)
    models = ollama_models()
    return {
        'session_id': context.session_id,
        'mode': 'read-only-model-discovery',
        'models': [{'name': item.name, 'size_bytes': item.size_bytes} for item in models],
        'applied': False,
    }


def qualify_local_roles(
    context: NativeContext,
    roles: tuple[str, ...],
    routing: OllamaRouting | None = None,
) -> dict[str, object]:
    """Discover installed Ollama models and verify selected serial role routing."""
    _validate_context(context)
    selected = routing if routing is not None else OllamaRouting()
    inventory = ollama_models(selected.base_url)
    result = qualify_models(selected, inventory, roles)
    result['session_id'] = context.session_id
    result['inference_executed'] = False
    result['worker_tools_qualified'] = False
    return result


def propose_and_review_local_coding(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    context: NativeContext,
    node: Node,
    worker_endpoint: LocalEndpoint,
    reviewer_endpoint: LocalEndpoint,
    transport: Transport,
    *,
    authorized: bool = False,
) -> dict[str, object]:
    """Return an independently reviewed proposal; do not apply any files."""
    _validate_context(context)
    if node.role != 'coding':
        msg = 'Only ordinary coding nodes supported by native model pipeline'
        raise WorkflowError(msg)
    result = generate_and_review(
        node.id,
        node.objective,
        node.owned_paths,
        worker_endpoint,
        reviewer_endpoint,
        transport,
        inference_authorized=authorized,
    )
    return {
        'session_id': context.session_id,
        'task_id': node.id,
        'mode': 'reviewed-proposal-only',
        'worker_model': worker_endpoint.model,
        'reviewer_model': reviewer_endpoint.model,
        'verdict': result.review.verdict,
        'findings': result.review.findings,
        'files': [{'path': item.path, 'sha256': item.sha256} for item in result.proposal.files],
        'applied': False,
        'authorizes_merge': False,
    }


def propose_and_review_local_replacements(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    context: NativeContext,
    node: Node,
    worker_endpoint: LocalEndpoint,
    reviewer_endpoint: LocalEndpoint,
    preimages: dict[str, str],
    transport: Transport,
    *,
    authorized: bool = False,
) -> dict[str, object]:
    """Review preimage-bound replacement proposals; never apply generated content."""
    _validate_context(context)
    if node.role != 'coding':
        msg = 'Replacement pipeline supports only the coding worker role'
        raise WorkflowError(msg)
    reviewed = generate_reviewed_replacements(
        worker_endpoint,
        reviewer_endpoint,
        node.id,
        node.objective,
        node.owned_paths,
        preimages,
        transport,
        authorized=authorized,
    )
    return {
        'session_id': context.session_id,
        'mode': 'reviewed-replacements-only',
        'task_id': node.id,
        'worker_model': worker_endpoint.model,
        'reviewer_model': reviewer_endpoint.model,
        'verdict': reviewed.verdict,
        'findings': reviewed.findings,
        'files': [
            {'path': item.path, 'expected_sha256': item.expected_sha256}
            for item in reviewed.replacements
        ],
        'applied': False,
        'authorizes_merge': False,
    }


def game_plan(context_or_file: NativeContext | Path, plan: Plan | None = None) -> dict[str, object]:
    """Keep established file-based skill CLI and context-based native API working."""
    if isinstance(context_or_file, NativeContext):
        if plan is None:
            msg = 'Native context requires a plan'
            raise WorkflowError(msg)
        return validate_native_plan(context_or_file, plan)
    if plan is not None:
        msg = 'File-based plan action takes no Plan argument'
        raise WorkflowError(msg)
    return _game_plan_file(context_or_file)
