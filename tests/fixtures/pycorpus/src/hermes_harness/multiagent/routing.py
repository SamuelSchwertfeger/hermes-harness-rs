"""Operator-observed Ollama role routing and serial capacity admission.

No model is automatically started or downloaded. Role selections are explicit
and model inventory is checked before inference, not inferred from file names.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.multiagent.local_inference import LocalEndpoint, LocalInferenceError

if TYPE_CHECKING:
    from hermes_harness.multiagent.ollama_discovery import OllamaModel

CYBER_MODEL = 'hf.co/mradermacher/Cyber-Ornith-1.5-9B-OBLITERATED-i1-GGUF:Q4_K_M'


@dataclass(frozen=True, slots=True)
class OllamaRouting:
    """Local model choices; all roles use the same bounded loopback endpoint."""

    coding: str = 'qwen2.5-coder:7b'
    lightweight: str = 'qwen2.5-coder:1.5b'
    reviewer: str = 'qwen3:4b'
    cyber_warrior: str = CYBER_MODEL
    base_url: str = 'http://127.0.0.1:11434'
    max_parallel: int = 1

    def endpoint(self, role: str) -> LocalEndpoint:
        if self.max_parallel != 1:
            msg = 'Concurrent inference requires separate GPU capacity qualification'
            raise LocalInferenceError(msg)
        roles = {
            'coding': self.coding,
            'lightweight': self.lightweight,
            'reviewer': self.reviewer,
            'cyber-warrior': self.cyber_warrior,
        }
        if role not in roles:
            msg = 'Unknown local model role'
            raise LocalInferenceError(msg)
        endpoint = LocalEndpoint(self.base_url.rstrip('/') + '/v1/chat/completions', roles[role])
        endpoint.validate()
        return endpoint


def qualify_models(
    routing: OllamaRouting, inventory: tuple[OllamaModel, ...], roles: tuple[str, ...]
) -> dict[str, object]:
    """Fail closed on missing installed models and misconfigured role separation."""
    available = {item.name for item in inventory}
    selected: dict[str, str] = {}
    for role in roles:
        endpoint = routing.endpoint(role)
        if endpoint.model not in available:
            msg = f'Model for role {role} is not installed: {endpoint.model}'
            raise LocalInferenceError(msg)
        selected[role] = endpoint.model
    if (
        'coding' in selected
        and 'reviewer' in selected
        and selected['coding'] == selected['reviewer']
    ):
        msg = 'Worker and reviewer must use distinct models'
        raise LocalInferenceError(msg)
    if (
        'cyber-warrior' in selected
        and 'reviewer' in selected
        and selected['cyber-warrior'] == selected['reviewer']
    ):
        msg = 'Cyber Warrior and reviewer must use distinct models'
        raise LocalInferenceError(msg)
    return {'roles': selected, 'max_parallel': 1, 'qualified_inventory_only': True}
