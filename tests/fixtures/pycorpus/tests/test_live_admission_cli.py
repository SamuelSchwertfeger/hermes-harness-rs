"""Static guardrails for the live probe's opt-in and no-auto-start boundary."""

import ast
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / 'scripts' / 'probe_live_admission.py'


def test_live_probe_requires_explicit_submit_flag() -> None:
    source = SCRIPT.read_text()
    tree = ast.parse(source)
    parser_function = next(
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name == 'build_parser'
    )
    submit_is_opt_in = any(
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr == 'add_argument'
        and node.args
        and isinstance(node.args[0], ast.Constant)
        and node.args[0].value == '--submit'
        and any(
            keyword.arg == 'action'
            and isinstance(keyword.value, ast.Constant)
            and keyword.value.value == 'store_true'
            for keyword in node.keywords
        )
        for node in ast.walk(parser_function)
    )
    assert submit_is_opt_in


def test_live_probe_attaches_to_discovered_endpoint_not_ensure_path() -> None:
    source = SCRIPT.read_text()
    assert 'discover_gateway_endpoint' in source
    assert 'attach_existing_gateway(config)' in source
    assert 'connect_gateway' in source
    assert 'ensure_gateway_runtime' not in source
    assert 'gateway.run' not in source


def test_live_probe_requires_exact_session_confirmation_and_disposable_ack() -> None:
    source = SCRIPT.read_text()
    assert 'args.confirm_session_id != args.session_id' in source
    assert 'if not args.confirm_disposable_session' in source
    assert "'--confirm-disposable-session is required for live submission'" in source
