"""Bounded, offline AST scan for Hermes submission/admission lookup candidates.

This reports source evidence only. It does not prove that a handler is registered,
reachable over RPC, authenticated, or safe to use for recovery.
"""

import argparse
import ast
import json
import os
from dataclasses import asdict, dataclass
from pathlib import Path

MAX_FILE_BYTES = 2_000_000
MAX_FILES = 5_000
MAX_TOTAL_BYTES = 50_000_000
MAX_FINDINGS = 500
SKIP_DIRS = frozenset({'.git', '.venv', '__pycache__', 'node_modules', '.mypy_cache'})
IDENTIFIER_TERMS = frozenset({'submission_id', 'admission_id', 'request_id'})
STRING_TERMS = ('submission_id', 'admission_id', 'prompt.receipt', 'prompt.submit')


class LookupProbeError(ValueError):
    """Raised when the source tree cannot be inspected within configured bounds."""


@dataclass(frozen=True, slots=True)
class ScanLimits:
    """Resource bounds for an offline source-tree inspection."""

    max_files: int = MAX_FILES
    max_file_bytes: int = MAX_FILE_BYTES
    max_total_bytes: int = MAX_TOTAL_BYTES

    def validate(self) -> None:
        if min(self.max_files, self.max_file_bytes, self.max_total_bytes) < 1:
            msg = 'Inspection limits must be positive'
            raise ValueError(msg)


@dataclass(frozen=True, slots=True)
class LookupFinding:
    """One function containing static lookup-related source evidence."""

    path: str
    line: int
    symbol: str
    identifiers: tuple[str, ...]
    string_literals: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class ParseFailure:
    """A Python file that could not be parsed by the current interpreter."""

    path: str
    line: int
    message: str


@dataclass(frozen=True, slots=True)
class LookupProbeReport:
    """Static scan output; findings are candidates, not verified RPC capabilities."""

    root: str
    files_scanned: int
    bytes_scanned: int
    findings: tuple[LookupFinding, ...]
    parse_failures: tuple[ParseFailure, ...]
    truncated: bool = False
    capability_verified: bool = False


class _CandidateVisitor(ast.NodeVisitor):
    def __init__(self, relative_path: str) -> None:
        self._relative_path = relative_path
        self._symbols: list[str] = []
        self.findings: list[LookupFinding] = []

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        self._symbols.append(node.name)
        self.generic_visit(node)
        self._symbols.pop()

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        self._visit_function(node)

    def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
        self._visit_function(node)

    def _visit_function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
        self._symbols.append(node.name)
        identifiers: set[str] = set()
        literals: set[str] = set()
        lines: list[int] = []
        for child in ast.walk(node):
            if isinstance(child, ast.Name) and child.id in IDENTIFIER_TERMS:
                identifiers.add(child.id)
                lines.append(child.lineno)
            elif isinstance(child, ast.arg) and child.arg in IDENTIFIER_TERMS:
                identifiers.add(child.arg)
                lines.append(child.lineno)
            elif isinstance(child, ast.Attribute) and child.attr in IDENTIFIER_TERMS:
                identifiers.add(child.attr)
                lines.append(child.lineno)
            elif isinstance(child, ast.Constant) and isinstance(child.value, str):
                value = child.value
                if any(term in value for term in STRING_TERMS):
                    literals.add(value[:160])
                    lines.append(child.lineno)
        if identifiers or literals or any(term in node.name for term in IDENTIFIER_TERMS):
            self.findings.append(
                LookupFinding(
                    path=self._relative_path,
                    line=min(lines, default=node.lineno),
                    symbol='.'.join(self._symbols),
                    identifiers=tuple(sorted(identifiers)),
                    string_literals=tuple(sorted(literals)),
                )
            )
        self.generic_visit(node)
        self._symbols.pop()


@dataclass(slots=True)
class _ScanState:
    findings: list[LookupFinding]
    parse_failures: list[ParseFailure]
    bytes_scanned: int = 0
    files_scanned: int = 0
    truncated: bool = False


def _eligible_directory(current: str, name: str) -> bool:
    path = Path(current) / name
    return name not in SKIP_DIRS and not name.startswith('.') and not path.is_symlink()


def _append_python_path(paths: list[Path], path: Path, max_files: int) -> None:
    if path.is_symlink():
        return
    paths.append(path)
    if len(paths) > max_files:
        msg = f'Source tree exceeds file limit ({max_files})'
        raise LookupProbeError(msg)


def _python_paths(base: Path, max_files: int) -> tuple[Path, ...]:
    paths: list[Path] = []
    for current, dirs, files in os.walk(base, topdown=True, followlinks=False):
        dirs[:] = sorted(name for name in dirs if _eligible_directory(current, name))
        for filename in sorted(files):
            if filename.endswith('.py'):
                _append_python_path(paths, Path(current) / filename, max_files)
    return tuple(paths)


def _inspect_file(path: Path, base: Path) -> tuple[tuple[LookupFinding, ...], ParseFailure | None]:
    relative = path.relative_to(base).as_posix()
    source = path.read_text(encoding='utf-8')
    try:
        tree = ast.parse(source, filename=relative)
    except SyntaxError as exc:
        return (), ParseFailure(relative, exc.lineno or 0, exc.msg)
    visitor = _CandidateVisitor(relative)
    visitor.visit(tree)
    return tuple(visitor.findings), None


def _scan_paths(paths: tuple[Path, ...], base: Path, limits: ScanLimits) -> _ScanState:
    state = _ScanState([], [])
    for path in paths:
        size = path.stat().st_size
        relative = path.relative_to(base).as_posix()
        if size > limits.max_file_bytes:
            state.parse_failures.append(ParseFailure(relative, 0, 'file exceeds size limit'))
            continue
        if state.bytes_scanned + size > limits.max_total_bytes:
            state.truncated = True
            break
        findings, failure = _inspect_file(path, base)
        state.bytes_scanned += size
        state.files_scanned += 1
        state.findings.extend(findings)
        if failure is not None:
            state.parse_failures.append(failure)
        if len(state.findings) >= MAX_FINDINGS:
            state.findings = state.findings[:MAX_FINDINGS]
            state.truncated = True
            break
    return state


def inspect_tree(root: Path, *, limits: ScanLimits | None = None) -> LookupProbeReport:
    """Scan an explicit source tree without importing or executing its Python files."""
    scan_limits = limits if limits is not None else ScanLimits()
    scan_limits.validate()
    base = root.resolve(strict=True)
    if not base.is_dir():
        msg = 'Source root must be a directory'
        raise LookupProbeError(msg)
    paths = _python_paths(base, scan_limits.max_files)
    state = _scan_paths(paths, base, scan_limits)
    return LookupProbeReport(
        root=str(base),
        files_scanned=state.files_scanned,
        bytes_scanned=state.bytes_scanned,
        findings=tuple(
            sorted(state.findings, key=lambda item: (item.path, item.line, item.symbol))
        ),
        parse_failures=tuple(state.parse_failures),
        truncated=state.truncated,
    )


def main() -> int:
    parser = argparse.ArgumentParser(
        description='Find static Hermes submission/admission lookup candidates without execution'
    )
    parser.add_argument('source_root', type=Path, help='Explicit Hermes Python source checkout')
    args = parser.parse_args()
    try:
        report = inspect_tree(args.source_root)
    except (OSError, UnicodeError, ValueError) as exc:
        parser.exit(2, f'Lookup inspection failed: {exc}\n')
    print(json.dumps(asdict(report), indent=2, sort_keys=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

# Temporary exports for legacy module import compatibility.
__all__ = [
    'IDENTIFIER_TERMS',
    'MAX_FILES',
    'MAX_FILE_BYTES',
    'MAX_FINDINGS',
    'MAX_TOTAL_BYTES',
    'SKIP_DIRS',
    'STRING_TERMS',
    'LookupFinding',
    'LookupProbeError',
    'LookupProbeReport',
    'ParseFailure',
    'Path',
    'ScanLimits',
    '_CandidateVisitor',
    '_ScanState',
    '_append_python_path',
    '_eligible_directory',
    '_inspect_file',
    '_python_paths',
    '_scan_paths',
    'argparse',
    'asdict',
    'ast',
    'dataclass',
    'inspect_tree',
    'json',
    'main',
    'os',
]
