"""Operator-only read-only cross-ledger inspection; never starts Hermes."""

import argparse
from pathlib import Path

from hermes_harness.operator.live_operator_report import LedgerPaths, inspect_task


def main() -> None:
    """Show evidence and uncertainty without modifying either ledger."""
    parser = argparse.ArgumentParser(description='Read-only supervised live task inspection')
    parser.add_argument('--bindings', required=True, type=Path)
    parser.add_argument('--services', required=True, type=Path)
    parser.add_argument('--admissions', required=True, type=Path)
    parser.add_argument('--task-id', required=True)
    args = parser.parse_args()
    report = inspect_task(LedgerPaths(args.bindings, args.services, args.admissions), args.task_id)
    print(report.as_json())


if __name__ == '__main__':
    main()

# Temporary exports for legacy module import compatibility.
__all__ = [
    'LedgerPaths',
    'Path',
    'argparse',
    'inspect_task',
    'main',
]
