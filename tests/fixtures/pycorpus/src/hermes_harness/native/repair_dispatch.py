"""One-shot, reservation-bound local replacement inference for failed tests.

A dispatch claim is created *before* either model request; interruptions remain
visible and are never retried automatically. No changes are applied or executed.
"""

import ast
import hashlib
import json
import socket
from contextlib import nullcontext
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.deployment.gpu import gpu_lane, load_gpu_policy
from hermes_harness.deployment.providers import load_registry
from hermes_harness.multiagent.local_inference import LocalEndpoint
from hermes_harness.multiagent.ollama_native import REVIEW_SCHEMA, NativeOllamaJSONTransport
from hermes_harness.multiagent.replacement_pipeline import generate_reviewed_replacements
from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.functional_verify import _audit_checkout
from hermes_harness.native.lifecycle import _files, _load
from hermes_harness.native.repair_controller import _require, _sequence, _write_once, inspect_repair

if TYPE_CHECKING:
    from hermes_harness.multiagent.local_inference import Transport

_REPLACEMENT_SCHEMA: dict[str, object] = {
    'type': 'object',
    'properties': {
        'task_id': {'type': 'string'},
        'replacements': {
            'type': 'array',
            'minItems': 1,
            'maxItems': 16,
            'items': {
                'type': 'object',
                'properties': {
                    'path': {'type': 'string'},
                    'expected_sha256': {'type': 'string'},
                    'content': {'type': 'string'},
                },
                'required': ['path', 'expected_sha256', 'content'],
                'additionalProperties': False,
            },
        },
    },
    'required': ['task_id', 'replacements'],
    'additionalProperties': False,
}


MAX_REPAIR_OBJECTIVE_LENGTH = 4096
MAX_REPAIR_ATTEMPTS = 3
MAX_REPAIR_FILES = 16
MAX_REPAIR_SOURCE_BYTES = 8192
MAX_REPAIR_CONTEXT_LENGTH = 12000


def inspect_dispatch(*, state: Path, task_id: str) -> dict[str, object]:
    """Inspect a dispatch without invoking either model."""
    root, _ = _sequence(state, task_id)
    report = inspect_repair(state=state, task_id=task_id)
    used = report['attempts_used']
    if not isinstance(used, int) or used == 0:
        return {'status': 'not-reserved', 'task_id': task_id}
    fence = root / f'{task_id}-repair-{used}-dispatch.json'
    review = root / f'{task_id}-repair-{used}-review.json'
    if not fence.exists() and not fence.is_symlink():
        return {'status': 'not-dispatched', 'task_id': task_id, 'number': used}
    claim, claim_hash = _load(fence)
    _require(
        claim.get('mode') == 'repair-dispatch-claim'
        and claim.get('task_id') == task_id
        and claim.get('number') == used,
        'Invalid dispatch claim',
    )
    if not review.exists() and not review.is_symlink():
        return {
            'status': 'dispatched-unresolved',
            'claim_sha256': claim_hash,
            'task_id': task_id,
            'number': used,
            'no_automatic_retry': True,
        }
    result, digest = _load(review)
    _require(
        result.get('mode') == 'repair-independent-review'
        and result.get('claim_sha256') == claim_hash
        and result.get('reservation_sha256') == claim.get('reservation_sha256')
        and result.get('task_id') == task_id
        and result.get('number') == used,
        'Invalid dispatch review linkage',
    )
    return {
        'status': 'review-recorded',
        'task_id': task_id,
        'number': used,
        'review': result,
        'review_sha256': digest,
        'review_path': str(review),
    }


def dispatch_repair(  # noqa: C901, PLR0912, PLR0913, PLR0914, PLR0915
    *,
    state: Path,
    task_id: str,
    checkpoint: Path,
    objective: str,
    owned_paths: tuple[str, ...],
    authorized: bool = False,
    worker: LocalEndpoint | None = None,
    reviewer: LocalEndpoint | None = None,
    transport: Transport | None = None,
    registry_file: Path | None = None,
    gpu_policy_file: Path | None = None,
) -> dict[str, object]:
    """Dispatch exactly once from the currently reserved failed-verification attempt."""
    _require(authorized, 'Explicit local model inference authorization required')
    _require(0 < len(objective) <= MAX_REPAIR_OBJECTIVE_LENGTH, 'Invalid repair objective')
    root, opened = _sequence(state, task_id)
    report = inspect_repair(state=state, task_id=task_id)
    _require(report['next_stage'] == 'await-attempt-disposition', 'No reserved repair attempt')
    number = report['attempts_used']
    _require(type(number) is int and 1 <= number <= MAX_REPAIR_ATTEMPTS, 'Invalid attempt number')
    reservation, reservation_hash = _load(root / f'{task_id}-repair-{number}.json')
    original, checkpoint_hash = _load(checkpoint)
    _require(
        checkpoint_hash == opened['checkpoint_sha256']
        and reservation.get('checkpoint_sha256') == checkpoint_hash,
        'Checkpoint does not match repair reservation',
    )
    _require(
        original.get('task_id') == task_id
        and original.get('baseline') == opened['baseline']
        and original.get('workspace') == opened['workspace'],
        'Repair identity mismatch',
    )
    workspace_name = original.get('workspace')
    _require(isinstance(workspace_name, str), 'Invalid worktree')
    if not isinstance(workspace_name, str):
        msg = 'Invalid worktree'
        raise WorkflowError(msg)
    workspace = Path(workspace_name)
    _require(workspace.is_dir() and not workspace.is_symlink(), 'Worktree unavailable')
    checkpoint_files = _files(original)
    _require(
        0 < len(owned_paths) <= MAX_REPAIR_FILES
        and len(set(owned_paths)) == len(owned_paths)
        and all(path in checkpoint_files for path in owned_paths),
        'Repair paths must be explicit existing checkpoint-owned files',
    )
    actual = _audit_checkout(workspace, original)
    _require(actual == checkpoint_files, 'Checkpoint worktree drift')
    preimages = {path: checkpoint_files[path] for path in owned_paths}
    # The replacement worker needs actual old contents, not only digest strings.
    # Pass them as untrusted task data, bounded by the model's context budget.
    source_context: dict[str, str] = {}
    for name in owned_paths:
        target = workspace / name
        _require(
            target.is_file()
            and not target.is_symlink()
            and target.resolve().is_relative_to(workspace.resolve()),
            'Unsafe repair source file',
        )
        raw = target.read_bytes()
        _require(len(raw) <= MAX_REPAIR_SOURCE_BYTES, 'Repair source exceeds context budget')
        _require(hashlib.sha256(raw).hexdigest() == preimages[name], 'Stale repair source preimage')
        try:
            source_context[name] = raw.decode('utf-8')
        except UnicodeError as exc:
            msg = 'Only UTF-8 repair source is supported'
            raise WorkflowError(msg) from exc
    _require(
        len(json.dumps(source_context)) <= MAX_REPAIR_CONTEXT_LENGTH,
        'Combined repair source exceeds context budget',
    )
    url = 'http://127.0.0.1:11434/v1/chat/completions'
    registry = load_registry(registry_file) if registry_file is not None else None
    policy = load_gpu_policy(gpu_policy_file) if gpu_policy_file is not None else None
    if registry is not None:  # noqa: PLR1702
        _require(
            worker is None and reviewer is None,
            'Explicit registry forbids unregistered endpoint overrides',
        )
        worker = worker or registry.endpoint('repair-worker')
        reviewer = reviewer or registry.endpoint('repair-reviewer')
        selected = (registry.role('repair-worker'), registry.role('repair-reviewer'))
        prompt_chars = len(objective) + len(json.dumps(source_context))
        _require(
            all(spec.context_tokens >= prompt_chars + 2048 + 512 for spec in selected),
            'Configured model context insufficient for bounded repair input',
        )
        remote_hosts = {entry.gpu_host for entry in selected if entry.gpu_host}
        if remote_hosts:
            _require(
                policy is not None and remote_hosts == {policy.host},
                'Remote GPU registry requires matching operator policy',
            )
            if policy is not None:
                policy.admit(tuple(entry.unit for entry in selected if entry.unit))
                if socket.gethostname() != policy.host:
                    _require(
                        not any(unit.managed for unit in policy.units),
                        'Remote services must remain externally managed; no distributed lease',
                    )
    worker = worker or LocalEndpoint(url, 'qwen2.5-coder:7b', 120, 2048)
    reviewer = reviewer or LocalEndpoint(url, 'qwen3:4b', 120, 512)
    _require(worker.model != reviewer.model, 'Worker/reviewer models must differ')
    worker.validate()
    reviewer.validate()
    if registry is not None and transport is None:
        transport = registry.transport(
            ('repair-worker', 'repair-reviewer'),
            {'repair-worker': _REPLACEMENT_SCHEMA, 'repair-reviewer': REVIEW_SCHEMA},
        )
    transport = transport or NativeOllamaJSONTransport(
        schemas_by_model={
            worker.model: _REPLACEMENT_SCHEMA,
            reviewer.model: REVIEW_SCHEMA,
        }
    )
    claim: dict[str, object] = {
        'mode': 'repair-dispatch-claim',
        'task_id': task_id,
        'number': number,
        'reservation_sha256': reservation_hash,
        'checkpoint_sha256': checkpoint_hash,
        'source_failure_sha256': opened['failure_sha256'],
        'baseline': opened['baseline'],
        'workspace': workspace_name,
        'owned_paths': list(owned_paths),
        'preimages': preimages,
        'objective_sha256': hashlib.sha256(objective.encode()).hexdigest(),
        'worker_model': worker.model,
        'reviewer_model': reviewer.model,
        'inference_granted': True,
        'application_granted': False,
        'execution_granted': False,
    }
    fence = root / f'{task_id}-repair-{number}-dispatch.json'
    try:
        claim_hash = _write_once(fence, claim)
    except FileExistsError as exc:
        msg = 'Repair attempt already dispatched; inspect saved evidence, never redispatch'
        raise WorkflowError(msg) from exc
    lane = (
        gpu_lane(policy)
        if policy is not None and socket.gethostname() == policy.host
        else nullcontext()
    )
    with lane:
        result = generate_reviewed_replacements(
            worker,
            reviewer,
            task_id,
            json.dumps({'objective': objective, 'existing_files': source_context}),
            owned_paths,
            preimages,
            transport,
            authorized=True,
        )
    # Detect any worktree change during inference, before persisting accepted evidence.
    _require(
        _audit_checkout(workspace, original) == checkpoint_files,
        'Worktree changed during inference; dispatch remains unresolved',
    )
    _require(
        all(
            item.path in preimages and item.expected_sha256 == preimages[item.path]
            for item in result.replacements
        ),
        'Replacement preimage mismatch',
    )
    # Keep genuine model review separate from bounded deterministic admission policy.
    unchanged = all(item.content == source_context[item.path] for item in result.replacements)
    no_op_accepted = unchanged and result.verdict == 'accept'
    syntax_failures: list[str] = []
    for item in result.replacements:
        if item.path.endswith('.py'):
            try:
                ast.parse(item.content, filename=item.path)
            except SyntaxError:
                syntax_failures.append(f'Invalid Python syntax in {item.path}')
    policy_rejected = no_op_accepted or bool(syntax_failures)
    effective_verdict = (
        'changes_required' if policy_rejected and result.verdict == 'accept' else result.verdict
    )
    policy_findings = [
        *(
            ['No-op replacement: all proposed contents match their preimages']
            if no_op_accepted
            else []
        ),
        *syntax_failures,
    ]
    evidence: dict[str, object] = {
        'mode': 'repair-independent-review',
        'task_id': task_id,
        'number': number,
        'claim_sha256': claim_hash,
        'reservation_sha256': reservation_hash,
        'checkpoint_sha256': checkpoint_hash,
        'source_failure_sha256': opened['failure_sha256'],
        'baseline': opened['baseline'],
        'workspace': workspace_name,
        'owned_paths': list(owned_paths),
        'worker_model': worker.model,
        'reviewer_model': reviewer.model,
        'verdict': effective_verdict,
        'model_verdict': result.verdict,
        'policy_rejected_no_op': no_op_accepted,
        'policy_rejected_invalid_syntax': bool(syntax_failures),
        'findings': [*result.findings, *policy_findings],
        'replacements': [
            {
                'path': item.path,
                'expected_sha256': item.expected_sha256,
                'content': item.content,
                'content_sha256': hashlib.sha256(item.content.encode()).hexdigest(),
            }
            for item in result.replacements
        ],
        'applied': False,
        'generated_code_executed': False,
        'human_decision_pending': True,
    }
    review_path = root / f'{task_id}-repair-{number}-review.json'
    digest = _write_once(review_path, evidence)
    return {**evidence, 'evidence': str(review_path), 'evidence_sha256': digest}
