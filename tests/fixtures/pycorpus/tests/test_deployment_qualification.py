"""Offline security-boundary tests for issues 24–27 (no live host changes)."""

import hashlib
import json
from dataclasses import asdict, replace
from pathlib import Path  # noqa: TC003
from typing import Self
from unittest.mock import MagicMock

import pytest

from hermes_harness.deployment.evaluation import EvalSample, evaluate_samples, load_samples
from hermes_harness.deployment.gpu import (
    GPUPolicy,
    GPUUnit,
    gpu_lane,
    switch_gpu_slot,
)
from hermes_harness.deployment.providers import (
    ConfiguredEndpoint,
    OpenAICompatibleTransport,
    ProviderError,
    ProviderRegistry,
    ProviderSpec,
    load_registry,
)
from hermes_harness.multiagent.runtime import WorkflowError


class _Response:
    status = 200

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_: object) -> None:
        pass

    def read(self, _limit: int) -> bytes:
        return b'{"id":"chatcmpl-1","model":"qwen35-2b","choices":[{"message":{"content":"OK"}}]}'


class _Opener:
    def __init__(self) -> None:
        self.headers: dict[str, str] = {}

    def open(self, request: object, timeout: float) -> _Response:
        assert timeout > 0
        self.headers = dict(request.header_items())  # type: ignore[attr-defined]
        return _Response()


def remote_spec() -> ProviderSpec:
    return ProviderSpec(
        'long',
        'llama.cpp',
        'qwen35-2b',
        'https://llm-long.example.home.arpa/v1/chat/completions',
        65536,
        'LOCAL_LLM_API_KEY',
        tool_calls=False,
        unit='llama-long.service',
        gpu_host='gpu-host-01',
    )


@pytest.mark.parametrize(
    'model',
    [
        'qwen2.5-coder:7b',
        'qwen3:8b',
        'hf.co/mradermacher/Cyber-Ornith-1.5-9B-OBLITERATED-i1-GGUF:Q4_K_M',
    ],
)
def test_ollama_registry_accepts_actual_tagged_model_ids(model: str) -> None:
    spec = ProviderSpec(
        'xps-coding',
        'ollama',
        model,
        'http://127.0.0.1:11435/v1/chat/completions',
        4096,
    )
    spec.validate()
    spec.admit_role('repair-worker')


@pytest.mark.parametrize('model', ['../model:7b', 'team//model:7b', 'model?token=x', ''])
def test_ollama_registry_rejects_unsafe_model_ids(model: str) -> None:
    spec = ProviderSpec(
        'xps-coding',
        'ollama',
        model,
        'http://127.0.0.1:11435/v1/chat/completions',
        4096,
    )
    with pytest.raises(ProviderError, match='invalid-provider-identity'):
        spec.validate()


def test_remote_https_validation_and_context() -> None:
    spec = remote_spec()
    spec.validate()
    with pytest.raises(ProviderError, match='tool-calling'):
        spec.admit_role('hermes-tool-worker')
    spec.admit_role('repair-worker')
    short = ProviderSpec(
        'short',
        'llama.cpp',
        'qwen8',
        'https://llm.example.home.arpa/v1/chat/completions',
        4096,
        'LOCAL_LLM_API_KEY',
        tool_calls=True,
    )
    with pytest.raises(ProviderError, match='insufficient-context'):
        short.admit_role('hermes-tool-worker')


@pytest.mark.parametrize(
    'url',
    [
        'http://llm.example.home.arpa/v1/chat/completions',
        'https://evil.example/v1/chat/completions',
        'https://user:secret@llm.example.home.arpa/v1/chat/completions',
        'https://llm.example.home.arpa/v1/chat/completions?key=123',
        'https://llm.example.home.arpa/admin',
    ],
)
def test_remote_url_fail_closed(url: str) -> None:
    spec = ProviderSpec('x', 'llama.cpp', 'm', url, 4096, 'KEY')
    with pytest.raises(ProviderError):
        spec.validate()


def test_registry_rejects_inline_secrets_and_duplicate_roles(tmp_path: Path) -> None:
    path = tmp_path / 'providers.json'
    path.write_text(
        json.dumps({
            'profiles': [{**asdict(remote_spec()), 'api_key': 'secret'}],
            'roles': {'repair-worker': 'long'},
        })
    )
    with pytest.raises(ProviderError):
        load_registry(path)


def test_remote_adapter_bearer_not_logged(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv('LOCAL_LLM_API_KEY', 'VERY_SECRET_VALUE')
    spec = remote_spec()
    endpoint = ConfiguredEndpoint(spec.url, spec.model, 30, 32, spec)
    fake = _Opener()
    data = OpenAICompatibleTransport(fake).complete(endpoint, b'{}')
    assert b'choices' in data
    assert fake.headers['Authorization'] == 'Bearer VERY_SECRET_VALUE'
    monkeypatch.delenv('LOCAL_LLM_API_KEY')
    with pytest.raises(ProviderError, match='credential-environment-missing'):
        OpenAICompatibleTransport(fake).complete(endpoint, b'{}')


def test_registry_roles_are_explicit() -> None:
    spec = remote_spec()
    registry = ProviderRegistry({'long': spec}, {'repair-reviewer': 'long'})
    assert registry.endpoint('repair-reviewer').model == 'qwen35-2b'
    with pytest.raises(ProviderError, match='role-not-configured'):
        registry.endpoint('repair-worker')


def policy(tmp_path: Path) -> GPUPolicy:
    return GPUPolicy(
        'gpu-host-01',
        8192,
        512,
        tmp_path / 'lane.lock',
        (
            GPUUnit('fallback.service', 1966, pinned=True, managed=False, model='qwen35'),
            GPUUnit('worker.service', 5230, pinned=False, managed=True, model='qwen8'),
            GPUUnit('granite.service', 5500, pinned=False, managed=True, model='granite'),
        ),
    )


def test_gpu_budget_and_protected_unit(tmp_path: Path) -> None:
    setting = policy(tmp_path)
    setting.admit(('worker.service',))
    with pytest.raises(WorkflowError, match='overcommit'):
        setting.admit(('worker.service', 'granite.service'))
    with pytest.raises(WorkflowError, match='overcommit'):
        setting.admit(('granite.service', 'worker.service'))


def test_gpu_lease_contention(tmp_path: Path) -> None:
    setting = policy(tmp_path)
    with gpu_lane(setting), pytest.raises(WorkflowError, match='busy'), gpu_lane(setting):
        pass
    with gpu_lane(setting):
        pass


def test_gpu_switch_rejects_pinned_and_unmanaged(tmp_path: Path) -> None:
    setting = policy(tmp_path)
    systemd = MagicMock()
    journal = tmp_path / 'journal'
    journal.mkdir()
    with pytest.raises(WorkflowError, match='Pinned or unmanaged'):
        switch_gpu_slot(
            policy=setting,
            prior='fallback.service',
            target='granite.service',
            transaction='test1',
            journal=journal,
            systemd=systemd,
            ready=lambda _: True,
            authorized=True,
        )
    systemd.stop.assert_not_called()


def test_gpu_switch_failure_records_unknown_without_retry(tmp_path: Path) -> None:
    setting = policy(tmp_path)
    journal = tmp_path / 'journal'
    journal.mkdir()
    systemd = MagicMock()
    systemd.active.side_effect = [True, False, False, False]
    systemd.start.side_effect = WorkflowError('failed')
    with pytest.raises(WorkflowError, match='UNKNOWN'):
        switch_gpu_slot(
            policy=setting,
            prior='worker.service',
            target='granite.service',
            transaction='txn1',
            journal=journal,
            systemd=systemd,
            ready=lambda _: True,
            authorized=True,
        )
    result = json.loads((journal / 'txn1-result.json').read_text())
    assert result['phase'] == 'UNKNOWN'
    assert result['manual_reconciliation_required'] is True
    with pytest.raises(WorkflowError):
        switch_gpu_slot(
            policy=setting,
            prior='worker.service',
            target='granite.service',
            transaction='txn1',
            journal=journal,
            systemd=systemd,
            ready=lambda _: True,
            authorized=True,
        )
    assert systemd.start.call_count == 1


def test_gpu_switch_success_requires_probed_ready(tmp_path: Path) -> None:
    setting = policy(tmp_path)
    journal = tmp_path / 'journal'
    journal.mkdir()
    systemd = MagicMock()
    systemd.active.side_effect = [True, False, False, True]
    record = switch_gpu_slot(
        policy=setting,
        prior='worker.service',
        target='granite.service',
        transaction='txn2',
        journal=journal,
        systemd=systemd,
        ready=lambda unit: unit.model == 'granite',
        authorized=True,
    )
    assert record['phase'] == 'READY'
    systemd.stop.assert_called_once_with('worker.service')
    systemd.start.assert_called_once_with('granite.service')


def test_evaluation_requires_real_bound_receipt(tmp_path: Path) -> None:
    response = {
        'tool_calls': [{'function': {'name': 'terminal', 'arguments': '{"command":"pwd"}'}}]
    }
    no_receipt = EvalSample('qwen', 'pwd', 's1', 'terminal', response)
    missing = evaluate_samples((no_receipt,))
    assert missing['results']['qwen']['categories']['missing_terminal_receipt'] == 1
    assert missing['results']['qwen']['tool_worker_eligible'] is False
    receipt = tmp_path / 'receipt.json'
    receipt.write_text(
        json.dumps({
            'session_id': 's1',
            'tool': 'terminal',
            'command': 'pwd',
            'executed': True,
            'exit_code': 0,
        })
    )
    valid = EvalSample(
        'qwen',
        'pwd',
        's1',
        'terminal',
        response,
        receipt,
        hashlib.sha256(receipt.read_bytes()).hexdigest(),
    )
    result = evaluate_samples((valid,))
    assert (
        result['results']['qwen']['categories']['reported_tool_success_requires_origin_review'] == 1
    )
    assert result['results']['qwen']['verified_successes'] == 0
    assert result['unattended_worker_admission'] is False


def test_evaluation_distinguishes_policy_denial_and_tool_schema() -> None:
    denied = EvalSample('granite', 'terminal', 's1', 'terminal', {}, policy_denied=True)
    bad = EvalSample(
        'granite',
        'terminal',
        's2',
        'terminal',
        {'tool_calls': [{'function': {'name': 'cat', 'arguments': '{"command":"pwd"}'}}]},
    )
    result = evaluate_samples((denied, bad))
    assert result['results']['granite']['categories'] == {
        'policy_denial': 1,
        'wrong_tool_name': 1,
    }


def test_evaluation_rejects_path_traversal(tmp_path: Path) -> None:
    file = tmp_path / 'samples.json'
    file.write_text(
        json.dumps([
            {
                'model': 'x',
                'task': 't',
                'session': 's',
                'expected_tool': 'terminal',
                'response': {},
                'receipt_file': '../secret',
            }
        ])
    )
    with pytest.raises(WorkflowError, match='filename'):
        load_samples(file, tmp_path)


def test_gpu_observed_capacity_rejection_keeps_unknown(tmp_path: Path) -> None:
    setting = policy(tmp_path)
    journal = tmp_path / 'journal'
    journal.mkdir()
    systemd = MagicMock()
    systemd.active.side_effect = [True, False, False]
    with pytest.raises(WorkflowError, match='UNKNOWN'):
        switch_gpu_slot(
            policy=setting,
            prior='worker.service',
            target='granite.service',
            transaction='low-vram',
            journal=journal,
            systemd=systemd,
            ready=lambda _: True,
            authorized=True,
            free_vram_mib=lambda: 128,
        )
    assert json.loads((journal / 'low-vram-result.json').read_text())['phase'] == 'UNKNOWN'
    systemd.start.assert_not_called()


def test_openai_completion_model_mismatch(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv('LOCAL_LLM_API_KEY', 'secret')

    class WrongModel(_Response):
        def read(self, _limit: int) -> bytes:
            return b'{"model":"other","choices":[{"message":{"content":"OK"}}]}'

    class WrongOpener(_Opener):
        def open(self, request: object, timeout: float) -> WrongModel:
            _ = request, timeout
            return WrongModel()

    spec = remote_spec()
    endpoint = ConfiguredEndpoint(spec.url, spec.model, 30, 32, spec)
    with pytest.raises(ProviderError, match='identity-or-schema'):
        OpenAICompatibleTransport(WrongOpener()).complete(endpoint, b'{}')


def test_bridge_parser_exposes_optin_deployment_commands(tmp_path: Path) -> None:
    from hermes_harness.native.skill_bridge import _parser  # noqa: PLC0415

    args = _parser().parse_args([
        'repair-dispatch',
        '--state',
        str(tmp_path / 'state'),
        '--task-id',
        'task',
        '--checkpoint',
        str(tmp_path / 'cp'),
        '--objective',
        'fix',
        '--owned-path',
        'x.py',
        '--session',
        's',
        '--provider-registry',
        str(tmp_path / 'providers.json'),
    ])
    assert args.provider_registry == tmp_path / 'providers.json'
    assert args.authorize_inference is False
    args = _parser().parse_args([
        'deploy-preflight',
        '--workspace',
        str(tmp_path),
        '--session',
        's',
    ])
    assert args.command == 'deploy-preflight'
    assert args.probe_network is False


def test_preflight_does_not_grant_native_session(tmp_path: Path) -> None:
    from hermes_harness.deployment.preflight import hp_preflight  # noqa: PLC0415

    result = hp_preflight(workspace=tmp_path)
    assert result['ready'] is False
    assert result['checks']['native_session']['status'] == 'BLOCK'
    assert result['checks']['context_64k']['status'] == 'BLOCK'
    assert result['gateway_modified'] is False


def test_repair_dispatch_uses_explicit_registry_without_sending_credentials(  # noqa: PLR0914, PLR0915
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from hermes_harness.multiagent.replace_edits import FileReplacement  # noqa: PLC0415
    from hermes_harness.multiagent.replacement_pipeline import ReplacementReview  # noqa: PLC0415
    from hermes_harness.native import repair_dispatch  # noqa: PLC0415
    from hermes_harness.native.repair_controller import open_repair, reserve_repair  # noqa: PLC0415

    monkeypatch.setenv('LOCAL_LLM_API_KEY', 'DO_NOT_LOG')
    state = tmp_path / 'evidence'
    state.mkdir()
    workspace = tmp_path / 'worktree'
    workspace.mkdir()
    (workspace / 'existing.py').write_text('print(0)\n')
    source_hash = hashlib.sha256(b'print(0)\n').hexdigest()
    checkpoint = tmp_path / 'checkpoint.json'
    record = {
        'mode': 'integrity-checkpoint',
        'task_id': 'one',
        'baseline': 'a' * 40,
        'workspace': str(workspace),
        'max_repair_attempts': 3,
        'files': [{'path': 'existing.py', 'sha256': source_hash}],
    }
    checkpoint.write_text(json.dumps(record))
    checkpoint_hash = hashlib.sha256(checkpoint.read_bytes()).hexdigest()
    failed = tmp_path / 'failed.json'
    failed.write_text(
        json.dumps({
            'mode': 'sandboxed-functional-verification',
            'task_id': 'one',
            'baseline': 'a' * 40,
            'workspace': str(workspace),
            'checkpoint_sha256': checkpoint_hash,
            'functional_verified': False,
            'tests_executed': True,
            'sandbox_started': True,
            'timed_out': False,
            'human_decision_pending': True,
        })
    )
    open_repair(failure=failed, checkpoint=checkpoint, state=state, authorized=True)
    reserve_repair(state=state, task_id='one', authorized=True)
    monkeypatch.setattr(
        repair_dispatch, '_audit_checkout', lambda _workspace, _record: {'existing.py': source_hash}
    )
    worker = ProviderSpec(
        'worker',
        'llama.cpp',
        'qwen8',
        'https://llm.example.home.arpa/v1/chat/completions',
        4096,
        'LOCAL_LLM_API_KEY',
    )
    reviewer = replace(remote_spec(), unit='', gpu_host='')
    registry = ProviderRegistry(
        {'worker': worker, 'reviewer': reviewer},
        {'repair-worker': 'worker', 'repair-reviewer': 'reviewer'},
    )
    monkeypatch.setattr(repair_dispatch, 'load_registry', lambda _: registry)
    invoked: list[tuple[object, ...]] = []

    def fake_generate(*args: object, **_kwargs: object) -> ReplacementReview:
        invoked.append(args)
        return ReplacementReview(
            'one',
            (FileReplacement('existing.py', source_hash, 'print(1)\n'),),
            reviewer.model,
            'accept',
            (),
        )

    monkeypatch.setattr(repair_dispatch, 'generate_reviewed_replacements', fake_generate)
    result = repair_dispatch.dispatch_repair(
        state=state,
        task_id='one',
        checkpoint=checkpoint,
        objective='fix',
        owned_paths=('existing.py',),
        authorized=True,
        registry_file=tmp_path / 'profile.json',
    )
    assert result['worker_model'] == 'qwen8'
    assert result['reviewer_model'] == 'qwen35-2b'
    assert invoked[0][0].model == 'qwen8'
    assert invoked[0][1].model == 'qwen35-2b'
    assert 'DO_NOT_LOG' not in (state / 'one-repair-1-review.json').read_text()
