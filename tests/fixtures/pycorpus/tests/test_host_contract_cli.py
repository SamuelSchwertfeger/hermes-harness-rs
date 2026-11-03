"""CLI entrypoint is inert until given an explicit profile home."""

import ast
from pathlib import Path


def test_host_script_syntax_and_explicit_endpoint() -> None:
    script = Path('scripts/validate_existing_hermes.py').read_text()
    tree = ast.parse(script)
    assert isinstance(tree, ast.Module)
    assert 'discover_gateway_endpoint' in script
    assert 'connect_gateway' in script
    assert 'ensure_gateway_runtime' not in script
    assert 'prompt.submit' not in script
    assert 'required=True' in script
