"""Small auditable CLI. Ticket state changes are local only."""

import argparse
import json
import sys
from dataclasses import asdict
from pathlib import Path

from hermes_harness.core.ollama import OllamaOptions, text_task
from hermes_harness.core.routing import Role, select
from hermes_harness.core.state import Stage, Ticket, create, load, save  # noqa: TC001
from hermes_harness.multiagent.cli import main as multiagent_main


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog='hermes-harness')
    parser.add_argument('--state', type=Path, default=Path('.harness-runtime'))
    sub = parser.add_subparsers(dest='action', required=True)
    new = sub.add_parser('new')
    new.add_argument('ticket')
    new.add_argument('summary')
    for action in ('show', 'baton'):
        sub.add_parser(action).add_argument('ticket')
    move = sub.add_parser('advance')
    move.add_argument('ticket')
    move.add_argument('stage', choices=[stage.value for stage in Stage])
    approve = sub.add_parser('approve')
    approve.add_argument('ticket')
    approve.add_argument('gate', choices=['plan', 'merge'])
    evidence = sub.add_parser('evidence')
    evidence.add_argument('ticket')
    evidence.add_argument(
        'key',
        choices=[
            'facts',
            'plan',
            'commit',
            'verification',
            'review',
            'ci',
            'merged_pr',
        ],
    )
    evidence.add_argument('value')
    sub.add_parser('route').add_argument('role', choices=[role.value for role in Role])
    sub.add_parser('local-scout').add_argument('prompt')
    return parser


def _baton(root: Path, ticket: Ticket) -> Path:
    path = root / 'tickets' / ticket.id / 'handoffs' / 'BATON.md'
    path.write_text(
        f'# Baton: {ticket.id}\n\nStage: {ticket.stage.value}\n\n'
        'Read ../state.json, ../facts.md, ../plan.md and briefs before proceeding.\n',
        encoding='utf-8',
    )
    return path


def _update(root: Path, args: argparse.Namespace, ticket: Ticket) -> bool:
    if args.action == 'advance':
        ticket.advance(Stage(args.stage))
    elif args.action == 'approve':
        ticket.approvals[args.gate] = True
    elif args.action == 'evidence':
        ticket.evidence[args.key] = args.value
    else:
        return False
    save(root, ticket)
    return True


def main() -> None:
    if len(sys.argv) > 1 and sys.argv[1] == 'multiagent':
        sys.argv.pop(1)
        multiagent_main()
        return
    args = _parser().parse_args()
    root: Path = args.state
    if args.action == 'route':
        print(json.dumps(asdict(select(Role(args.role))), indent=2, default=str))
        return
    if args.action == 'local-scout':
        print(text_task(args.prompt, options=OllamaOptions(lock_file=root / 'gpu.lock')))
        return
    ticket = (
        create(root, args.ticket, args.summary) if args.action == 'new' else load(root, args.ticket)
    )
    if args.action == 'baton':
        print(_baton(root, ticket))
        return
    _update(root, args, ticket)
    print(
        json.dumps(
            {
                'id': ticket.id,
                'stage': ticket.stage.value,
                'evidence': ticket.evidence,
            },
            indent=2,
        )
    )


if __name__ == '__main__':
    main()
