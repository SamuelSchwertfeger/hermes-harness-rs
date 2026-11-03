"""Host-observed model routing policy: no live inference in tests."""

import pytest

from hermes_harness.multiagent.local_inference import LocalInferenceError
from hermes_harness.multiagent.ollama_discovery import OllamaModel
from hermes_harness.multiagent.routing import CYBER_MODEL, OllamaRouting, qualify_models


def test_routing_defaults_match_observed_installation() -> None:
    routing = OllamaRouting()
    assert routing.endpoint('coding').model == 'qwen2.5-coder:7b'
    assert routing.endpoint('cyber-warrior').model == CYBER_MODEL
    assert routing.endpoint('reviewer').model == 'qwen3:4b'


def test_inventory_validation_is_fail_closed() -> None:
    routing = OllamaRouting()
    models = (
        OllamaModel('qwen2.5-coder:7b', 4_700_000_000),
        OllamaModel('qwen3:4b', 2_500_000_000),
    )
    result = qualify_models(routing, models, ('coding', 'reviewer'))
    assert result['max_parallel'] == 1
    with pytest.raises(LocalInferenceError, match='not installed'):
        qualify_models(routing, models, ('cyber-warrior',))


def test_unqualified_parallelism_is_rejected() -> None:
    with pytest.raises(LocalInferenceError, match='capacity qualification'):
        OllamaRouting(max_parallel=2).endpoint('coding')


def test_review_identity_cannot_overlap() -> None:
    routing = OllamaRouting(reviewer='qwen2.5-coder:7b')
    models = (OllamaModel('qwen2.5-coder:7b', 100),)
    with pytest.raises(LocalInferenceError, match='distinct models'):
        qualify_models(routing, models, ('coding', 'reviewer'))
