"""Evidence-based model comparison; never turns a claimed tool call into execution.

Hermes-origin terminal receipts must be collected from real approved sessions.
This analyzer is offline and never launches arbitrary model code or host commands.
"""

import hashlib
import json
from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.multiagent.runtime import WorkflowError

if TYPE_CHECKING:
    from pathlib import Path

MIN_TOOL_ATTEMPTS = 5
MIN_TOOL_SUCCESS_FRACTION = 0.95
MAX_TRACES = 512
MAX_TRACE_BYTES = 128 * 1024


@dataclass(frozen=True, slots=True)
class EvalSample:
    model: str
    task: str
    session: str
    expected_tool: str
    response: dict[str, object]
    receipt_path: Path | None = None
    receipt_sha256: str = ''
    policy_denied: bool = False
    duration_ms: int = 0
    input_tokens: int = 0
    output_tokens: int = 0


def _receipt(sample: EvalSample) -> tuple[str, bool]:  # noqa: C901, PLR0911, PLR0912
    if sample.policy_denied:
        return 'policy_denial', False
    message = sample.response
    calls = message.get('tool_calls')
    if not isinstance(calls, list) or len(calls) != 1 or not isinstance(calls[0], dict):
        return 'missing_or_malformed_tool_call', False
    call = calls[0]
    function = call.get('function')
    if not isinstance(function, dict):
        return 'malformed_tool_schema', False
    if function.get('name') != sample.expected_tool:
        return 'wrong_tool_name', False
    arguments = function.get('arguments')
    try:
        decoded = json.loads(arguments) if isinstance(arguments, str) else arguments
    except ValueError, TypeError:
        return 'malformed_tool_arguments', False
    if not isinstance(decoded, dict) or not isinstance(decoded.get('command'), str):
        return 'malformed_tool_arguments', False
    if sample.receipt_path is None or not sample.receipt_sha256:
        return 'missing_terminal_receipt', False
    path = sample.receipt_path
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_TRACE_BYTES:
        return 'invalid_terminal_receipt', False
    content = path.read_bytes()
    if hashlib.sha256(content).hexdigest() != sample.receipt_sha256:
        return 'tampered_terminal_receipt', False
    try:
        receipt = json.loads(content)
    except ValueError:
        return 'invalid_terminal_receipt', False
    if not isinstance(receipt, dict) or (  # noqa: PLR0916
        receipt.get('session_id') != sample.session
        or receipt.get('command') != decoded['command']
        or receipt.get('tool') != sample.expected_tool
    ):
        return 'terminal_receipt_binding_mismatch', False
    if receipt.get('policy_denied') is True:
        return 'policy_denial', False
    if receipt.get('executed') is not True:
        return 'unconfirmed_tool_execution', False
    if receipt.get('exit_code') != 0:
        return 'tool_execution_error', False
    # Source origin (Hermes session) still requires operator inspection of the receipt.
    return 'reported_tool_success_requires_origin_review', False


def _checked_counter(value: object) -> int:
    """Validate counter values held in the heterogeneous report mapping."""
    if not isinstance(value, int):
        msg = 'Invalid evaluation counter'
        raise WorkflowError(msg)
    return value


def evaluate_samples(samples: tuple[EvalSample, ...]) -> dict[str, object]:  # noqa: PLR0915
    """No automatic tool-worker admission solely from externally supplied JSON."""
    if not samples or len(samples) > MAX_TRACES:
        msg = 'Evaluation sample count invalid'
        raise WorkflowError(msg)
    matrix: dict[str, dict[str, object]] = {}
    for item in samples:
        if not item.model or not item.session or item.duration_ms < 0:
            msg = 'Invalid evaluation sample identity'
            raise WorkflowError(msg)
        status, qualified = _receipt(item)
        row = matrix.setdefault(
            item.model,
            {
                'total': 0,
                'categories': {},
                'verified_successes': 0,
                'duration_ms': 0,
                'input_tokens': 0,
                'output_tokens': 0,
            },
        )
        row['total'] = _checked_counter(row['total']) + 1
        categories = row['categories']
        if not isinstance(categories, dict):
            msg = 'Invalid evaluation state'
            raise WorkflowError(msg)
        categories[status] = categories.get(status, 0) + 1
        row['verified_successes'] = _checked_counter(row['verified_successes']) + int(qualified)
        row['duration_ms'] = _checked_counter(row['duration_ms']) + item.duration_ms
        row['input_tokens'] = _checked_counter(row['input_tokens']) + item.input_tokens
        row['output_tokens'] = _checked_counter(row['output_tokens']) + item.output_tokens
    for row in matrix.values():
        total = _checked_counter(row['total'])
        successes = _checked_counter(row['verified_successes'])
        row['success_fraction'] = successes / total
        row['meets_sample_floor'] = total >= MIN_TOOL_ATTEMPTS
        row['tool_worker_eligible'] = (
            total >= MIN_TOOL_ATTEMPTS and successes / total >= MIN_TOOL_SUCCESS_FRACTION
        )
        row['operator_origin_review_required'] = True
    return {
        'mode': 'offline-model-evaluation',
        'samples': len(samples),
        'results': matrix,
        'generated_code_executed': False,
        'gateway_modified': False,
        'unattended_worker_admission': False,
    }


def load_samples(path: Path, evidence_root: Path) -> tuple[EvalSample, ...]:  # noqa: C901, PLR0912, PLR0915
    if (
        evidence_root.is_symlink()  # noqa: PLR0916
        or not evidence_root.is_dir()
        or path.is_symlink()
        or not path.is_file()
        or path.stat().st_size > MAX_TRACE_BYTES
    ):
        msg = 'Invalid evaluation fixture path'
        raise WorkflowError(msg)
    try:
        raw = json.loads(path.read_text())
    except ValueError as exc:
        msg = 'Malformed evaluation fixture'
        raise WorkflowError(msg) from exc
    if not isinstance(raw, list):
        msg = 'Evaluation fixtures must be a list'
        raise WorkflowError(msg)
    results: list[EvalSample] = []
    for item in raw:
        if not isinstance(item, dict):
            msg = 'Invalid evaluation entry'
            raise WorkflowError(msg)
        receipt_name = item.get('receipt_file')
        receipt_path = None
        if receipt_name is not None:
            if not isinstance(receipt_name, str) or '/' in receipt_name or '\\' in receipt_name:
                msg = 'Evaluation receipt path must be a filename'
                raise WorkflowError(msg)
            receipt_path = evidence_root / receipt_name
        if not isinstance(item.get('response'), dict):
            msg = 'Evaluation response must be an object'
            raise WorkflowError(msg)
        try:
            results.append(
                EvalSample(
                    model=item['model'],
                    task=item['task'],
                    session=item['session'],
                    expected_tool=item['expected_tool'],
                    response=item['response'],
                    receipt_path=receipt_path,
                    receipt_sha256=item.get('receipt_sha256', ''),
                    policy_denied=item.get('policy_denied', False),
                    duration_ms=item.get('duration_ms', 0),
                    input_tokens=item.get('input_tokens', 0),
                    output_tokens=item.get('output_tokens', 0),
                )
            )
        except (TypeError, KeyError) as exc:
            msg = 'Invalid evaluation sample fields'
            raise WorkflowError(msg) from exc
    return tuple(results)
