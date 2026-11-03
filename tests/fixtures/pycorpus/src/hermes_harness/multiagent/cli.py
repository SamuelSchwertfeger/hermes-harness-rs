"""Explicitly offline multi-task operator commands; no live execution entrypoint."""

import argparse
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

from hermes_harness.multiagent.runtime import (
    DemoFactory,
    ExecutionConfig,
    Node,
    Plan,
    decide,
    execute,
    inspect,
    ordered_nodes,
)


def _demo() -> dict[str, object]:
    with tempfile.TemporaryDirectory(prefix='hermes-offline-demo-') as directory:
        root = Path(directory)
        repo = root / 'source'
        repo.mkdir()
        for args in (
            ('init', '-q', '-b', 'dev'),
            (
                '-c',
                'user.name=Offline Demo',
                '-c',
                'user.email=demo@example.invalid',
                'commit',
                '--allow-empty',
                '-qm',
                'baseline',
            ),
        ):
            _git(repo, args)
        state = root / 'state'
        state.mkdir()
        plan = Plan(
            'demo',
            'Two bounded offline tasks',
            (
                Node('first', 'Create first evidence', ('first.txt',)),
                Node('second', 'Create second evidence', ('second.txt',), ('first',)),
            ),
        )
        result = execute(state, plan, ExecutionConfig(repo, DemoFactory()))
        return {
            'mode': 'offline',
            'state': result['state'],
            'tasks': result['tasks'],
            'decision': result['decision'],
            'note': 'Temporary demonstration removed; no live provider or merge executed',
        }


def _git(repo: Path, arguments: tuple[str, ...]) -> None:
    binary = shutil.which('git')
    if binary is None:
        msg = 'Git executable not available'
        raise RuntimeError(msg)
    subprocess.run(  # noqa: S603 - fixed local Git command with bounded arguments
        [str(Path(binary).resolve()), '-C', str(repo), *arguments],
        check=True,
        capture_output=True,
        shell=False,
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog='hermes-harness multiagent')
    parser.add_argument('--state', type=Path, default=Path('.harness-multiagent'))
    command = parser.add_subparsers(dest='action', required=True)
    command.add_parser('demo')
    plan_cmd = command.add_parser('plan')
    plan_cmd.add_argument('file', type=Path, help='JSON plan, see docs/multiagent-runtime.md')
    run = command.add_parser('run-offline')
    run.add_argument('file', type=Path)
    run.add_argument('--repo', type=Path, required=True)
    for name in ('status', 'inspect', 'reconcile', 'cancel'):
        command.add_parser(name).add_argument('run_id')
    decision = command.add_parser('decision')
    decision.add_argument('run_id')
    decision.add_argument('choice', choices=('approve', 'reject', 'request-changes'))
    command.add_parser('doctor')
    return parser


def _load_plan(path: Path) -> Plan:
    document = json.loads(path.read_text(encoding='utf-8'))
    return Plan(
        str(document['id']),
        str(document['objective']),
        tuple(
            Node(
                str(item['id']),
                str(item['objective']),
                tuple(item['owned_paths']),
                tuple(item.get('dependencies', ())),
            )
            for item in document['nodes']
        ),
        str(document.get('primary_identity', 'offline-primary')),
    )


def _run_plan(args: argparse.Namespace) -> dict[str, object]:
    plan = _load_plan(args.file)
    nodes = ordered_nodes(plan)
    if args.action == 'plan':
        return {'ordered_nodes': [node.id for node in nodes], 'mode': 'offline', 'parallel': False}
    args.state.mkdir(mode=0o700, parents=True, exist_ok=True)
    return execute(args.state, plan, ExecutionConfig(args.repo, DemoFactory()))


def _run_command(args: argparse.Namespace) -> dict[str, object]:
    if args.action == 'demo':
        return _demo()
    if args.action == 'doctor':
        return {
            'offline': True,
            'live_gateway': 'unqualified',
            'provider_execution': 'disabled',
            'concurrency': 1,
            'command_isolation': 'not provided by offline doubles',
        }
    if args.action in ('plan', 'run-offline'):
        return _run_plan(args)
    if args.action == 'decision':
        return decide(args.state, args.run_id, args.choice)
    return _inspection_command(args)


def _inspection_command(args: argparse.Namespace) -> dict[str, object]:
    result = inspect(args.state, args.run_id)
    if args.action == 'cancel':
        result['cancel_note'] = (
            'No running background process exists; stop is fail-closed. '
            'Existing evidence retained; no resubmission.'
        )
    return result


def main() -> None:
    args = _parser().parse_args()
    print(json.dumps(_run_command(args), indent=2))


if __name__ == '__main__':
    main()
