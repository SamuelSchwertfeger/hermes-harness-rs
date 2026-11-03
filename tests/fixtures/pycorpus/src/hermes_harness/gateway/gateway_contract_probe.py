"""Read-only, offline inspection of Hermes gateway admission RPC source.

This reports static hints, not proof of on-wire schema or cancellation guarantees.
No Hermes modules are imported, and no gateway process is started.
"""

import argparse
import ast
import json
from dataclasses import asdict, dataclass
from pathlib import Path

SOURCE_RELATIVE = Path('gateway/session_controls.py')
TARGETS = ('submit', 'receipt', 'cancel')
MAX_SOURCE_BYTES = 2_000_000


class ContractInspectionError(ValueError):
    """Raised when source cannot be inspected safely or unambiguously."""


@dataclass(frozen=True, slots=True)
class MethodHint:
    name: str
    line: int
    parameters: tuple[str, ...]
    literal_dict_keys: tuple[str, ...]
    rpc_names: tuple[str, ...]
    returns: int


@dataclass(frozen=True, slots=True)
class InspectionReport:
    source: str
    methods: tuple[MethodHint, ...]
    verified: bool = False
    live_dispatch_enabled: bool = False


def _literal_keys(function: ast.FunctionDef | ast.AsyncFunctionDef) -> tuple[str, ...]:
    keys: set[str] = set()
    for node in ast.walk(function):
        if isinstance(node, ast.Dict):
            keys.update(
                key.value
                for key in node.keys
                if isinstance(key, ast.Constant) and isinstance(key.value, str)
            )
    return tuple(sorted(keys))


def _rpc_names(function: ast.FunctionDef | ast.AsyncFunctionDef) -> tuple[str, ...]:
    names: set[str] = set()
    for node in ast.walk(function):
        if (
            isinstance(node, ast.Constant)
            and isinstance(node.value, str)
            and node.value.startswith(('prompt.', 'session.'))
        ):
            names.add(node.value)
    return tuple(sorted(names))


def _hint(function: ast.FunctionDef | ast.AsyncFunctionDef) -> MethodHint:
    args = function.args
    parameters = tuple(arg.arg for arg in (*args.posonlyargs, *args.args, *args.kwonlyargs))
    returns = sum(isinstance(node, ast.Return) for node in ast.walk(function))
    return MethodHint(
        function.name,
        function.lineno,
        parameters,
        _literal_keys(function),
        _rpc_names(function),
        returns,
    )


def inspect_source(source: str, *, source_name: str = '<fixture>') -> InspectionReport:
    """Collect source hints; reject absent or duplicate target handlers."""
    try:
        tree = ast.parse(source, filename=source_name)
    except SyntaxError as exc:
        msg = f'Cannot parse Hermes gateway source: {exc.msg}'
        raise ContractInspectionError(msg) from exc
    found: dict[str, list[MethodHint]] = {name: [] for name in TARGETS}
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name in found:
            found[node.name].append(_hint(node))
    wrong = tuple(name for name, hints in found.items() if len(hints) != 1)
    if wrong:
        msg = f'Expected exactly one definition of each handler; ambiguous: {", ".join(wrong)}'
        raise ContractInspectionError(msg)
    return InspectionReport(source_name, tuple(found[name][0] for name in TARGETS))


def inspect_checkout(checkout: Path) -> InspectionReport:
    """Read only the expected handler file in an explicitly supplied checkout."""
    base = checkout.resolve(strict=True)
    target = base / SOURCE_RELATIVE
    if target.is_symlink() or not target.is_file():
        msg = f'Missing or symlinked source: {SOURCE_RELATIVE}'
        raise ContractInspectionError(msg)
    if not target.resolve().is_relative_to(base):
        msg = 'Source path escaped checkout'
        raise ContractInspectionError(msg)
    if target.stat().st_size > MAX_SOURCE_BYTES:
        msg = 'Source file exceeds offline inspection limit'
        raise ContractInspectionError(msg)
    return inspect_source(target.read_text(encoding='utf-8'), source_name=str(SOURCE_RELATIVE))


def main() -> int:
    parser = argparse.ArgumentParser(
        description='Inspect Hermes RPC handlers without running Hermes'
    )
    parser.add_argument('checkout', type=Path, help='Hermes source checkout root')
    args = parser.parse_args()
    try:
        report = inspect_checkout(args.checkout)
    except (OSError, UnicodeError, ContractInspectionError) as exc:
        parser.exit(2, f'Inspection failed: {exc}\n')
    print(json.dumps(asdict(report), indent=2, sort_keys=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_SOURCE_BYTES',
    'SOURCE_RELATIVE',
    'TARGETS',
    'ContractInspectionError',
    'InspectionReport',
    'MethodHint',
    'Path',
    '_hint',
    '_literal_keys',
    '_rpc_names',
    'argparse',
    'asdict',
    'ast',
    'dataclass',
    'inspect_checkout',
    'inspect_source',
    'json',
    'main',
]
