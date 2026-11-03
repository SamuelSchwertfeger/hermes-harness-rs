"""Hermes skill CLI and local-model adapters (host qualification required)."""

from hermes_harness.native.workflows import (
    NativeContext,
    admit_plan_json,
    agents_assemble_offline,
    apply_local_new_file_proposal,
    discover_local_ollama_models,
    game_plan,
    propose_and_review_local_coding,
    propose_and_review_local_replacements,
    propose_local_edits,
    propose_local_plan,
    qualify_local_roles,
    what_we_know,
)

__all__ = [
    'NativeContext',
    'admit_plan_json',
    'agents_assemble_offline',
    'apply_local_new_file_proposal',
    'discover_local_ollama_models',
    'game_plan',
    'propose_and_review_local_coding',
    'propose_and_review_local_replacements',
    'propose_local_edits',
    'propose_local_plan',
    'qualify_local_roles',
    'what_we_know',
]
