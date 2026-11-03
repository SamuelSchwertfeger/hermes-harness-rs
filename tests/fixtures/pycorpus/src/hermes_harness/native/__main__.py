"""Diagnostic invocation of Hermes-native skill backend actions."""

import argparse
import json
import sys
from pathlib import Path

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.workflows import assemble, game_plan, inspect_run, investigate


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog='python -m hermes_harness.native')
    sub = parser.add_subparsers(dest='action', required=True)
    sub.add_parser('lets-investigate').add_argument('--repo', type=Path, required=True)
    sub.add_parser('game-plan').add_argument('--plan', type=Path, required=True)
    run = sub.add_parser('agents-assemble')
    run.add_argument('--repo', type=Path, required=True)
    run.add_argument('--plan', type=Path, required=True)
    run.add_argument('--state', type=Path, required=True)
    run.add_argument('--confirm-offline', action='store_true')
    status = sub.add_parser('inspect')
    status.add_argument('--state', type=Path, required=True)
    status.add_argument('--run-id', required=True)
    return parser


def _invoke(args: argparse.Namespace) -> dict[str, object]:
    if args.action == 'lets-investigate':
        return investigate(args.repo)
    if args.action == 'game-plan':
        return game_plan(args.plan)
    if args.action == 'agents-assemble':
        if not args.confirm_offline:
            msg = 'Explicit --confirm-offline is required'
            raise WorkflowError(msg)
        return assemble(args.repo, args.state, args.plan)
    return inspect_run(args.state, args.run_id)


def main() -> int:
    args = _parser().parse_args()
    try:
        result = _invoke(args)
    except (WorkflowError, OSError, ValueError, KeyError) as exc:
        print(json.dumps({'error': type(exc).__name__, 'message': str(exc)}), file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
