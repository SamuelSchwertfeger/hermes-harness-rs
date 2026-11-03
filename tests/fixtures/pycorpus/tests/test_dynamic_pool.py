"""On-demand XPS model pool authorization and inventory tests."""

from unittest.mock import patch

import pytest

from hermes_harness.multiagent.dynamic_pool import available_models, infer
from hermes_harness.multiagent.local_inference import LocalInferenceError
from hermes_harness.multiagent.ollama_discovery import OllamaModel


def test_any_installed_model_can_be_discovered() -> None:
    models = (
        OllamaModel('granite4:3b', 200),
        OllamaModel('qwen2.5-coder:7b', 300),
        OllamaModel('hf.co/org/cyber:Q4_K_M', 500),
    )
    with patch('hermes_harness.multiagent.dynamic_pool.ollama_models', return_value=models):
        assert available_models() == tuple(model.name for model in models)


def test_operator_approval_required() -> None:
    with pytest.raises(LocalInferenceError, match='authorization'):
        infer('granite4:3b', 'test')


def test_unavailable_model_cannot_run() -> None:
    with (
        patch('hermes_harness.multiagent.dynamic_pool.ollama_models', return_value=()),
        pytest.raises(LocalInferenceError, match='unavailable'),
    ):
        infer('granite4:3b', 'test', authorized=True)


def test_only_loopback_xps_inventory() -> None:
    with pytest.raises(LocalInferenceError, match='Unapproved'):
        available_models('http://example.com')
