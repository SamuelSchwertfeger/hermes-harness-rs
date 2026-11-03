"""Write tests/fixtures/pysource_parity.json for the pysource differential test.

Per file in the vendored corpus (tests/fixtures/pycorpus/{src,tests}) and the synthetic tricky file, record the facts
`gateway/gateway_contract_probe.py` extracts per function, using the same helper logic
(`_literal_keys`, `_rpc_names`, `_hint`), for EVERY function and async function.
Run under CPython 3.14 from the repo root:
    uv run --no-project --python 3.14 python -I tools/gen_contract_fixture.py
"""

import ast
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Vendored copy of the original Python sources (byte-identical, see .gitattributes).
UPSTREAM = ROOT / 'tests' / 'fixtures' / 'pycorpus'
OUT = ROOT / 'tests' / 'fixtures' / 'pysource_parity.json'
TRICKY = ROOT / 'tests' / 'fixtures' / 'pysource_tricky.py'
FUNCTIONS = (ast.FunctionDef, ast.AsyncFunctionDef)


# Copied verbatim from upstream gateway/gateway_contract_probe.py.
def _literal_keys(function: ast.AST) -> tuple[str, ...]:
    keys: set[str] = set()
    for node in ast.walk(function):
        if isinstance(node, ast.Dict):
            keys.update(
                key.value
                for key in node.keys
                if isinstance(key, ast.Constant) and isinstance(key.value, str)
            )
    return tuple(sorted(keys))


def _rpc_names(function: ast.AST) -> tuple[str, ...]:
    names: set[str] = set()
    for node in ast.walk(function):
        if (
            isinstance(node, ast.Constant)
            and isinstance(node.value, str)
            and node.value.startswith(('prompt.', 'session.'))
        ):
            names.add(node.value)
    return tuple(sorted(names))


def _hint(function: ast.FunctionDef | ast.AsyncFunctionDef) -> dict[str, object]:
    args = function.args
    parameters = tuple(arg.arg for arg in (*args.posonlyargs, *args.args, *args.kwonlyargs))
    returns = sum(isinstance(node, ast.Return) for node in ast.walk(function))
    return {
        'name': function.name,
        'line': function.lineno,
        'parameters': list(parameters),
        'literal_dict_keys': list(_literal_keys(function)),
        'rpc_names': list(_rpc_names(function)),
        'returns': returns,
    }


def normalize(data: bytes) -> str:
    return data.decode('utf-8').replace('\r\n', '\n')


def facts(text: str) -> list[dict[str, object]] | None:
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return None
    hints = [_hint(n) for n in ast.walk(tree) if isinstance(n, FUNCTIONS)]
    return sorted(hints, key=lambda h: (h['line'], h['name']))


def main() -> None:
    files = {}
    paths = [TRICKY]
    for sub in ('src', 'tests'):
        paths.extend(sorted(p for p in (UPSTREAM / sub).rglob('*.py') if 'fixtures' not in p.relative_to(UPSTREAM).parts))
    for path in paths:
        data = path.read_bytes()
        text = normalize(data)
        key = path.name if path == TRICKY else path.relative_to(UPSTREAM).as_posix()
        files[key] = {
            'sha256': hashlib.sha256(text.encode('utf-8')).hexdigest(),
            'functions': facts(text),
        }
    tricky_text = normalize(TRICKY.read_bytes())
    if facts(tricky_text.replace('\n', '\r\n')) != facts(tricky_text):
        msg = 'CRLF variant of the tricky file differs under CPython'
        raise SystemExit(msg)
    total = sum(len(f['functions'] or []) for f in files.values())
    OUT.write_text(json.dumps(files, indent=1, ensure_ascii=True) + '\n', encoding='utf-8')
    print(f'{len(files)} files, {total} functions', file=sys.stderr)


if __name__ == '__main__':
    main()
