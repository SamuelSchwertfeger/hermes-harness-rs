"""Bounded JSON command bridge invoked through Hermes's existing terminal tool.

This is NOT a new Hermes slash-command router. Skills call this diagnostic,
read-mostly backend entrypoint from the originating Hermes session.
"""

import argparse
import json
from pathlib import Path

from hermes_harness.deployment.bridge import add_deployment_commands, handle_deployment
from hermes_harness.multiagent.plan_io import parse_plan_json
from hermes_harness.multiagent.runtime import WorkflowError, inspect, ordered_nodes

MAX_FILE_SIZE = 65536


def _parser() -> argparse.ArgumentParser:  # noqa: PLR0914, PLR0915 - native bridge registration
    parser = argparse.ArgumentParser(prog='hermes-harness-skill-bridge')
    sub = parser.add_subparsers(dest='command', required=True)
    add_deployment_commands(sub)
    sub.add_parser('doctor')
    pool = sub.add_parser('xps-models')
    pool.add_argument('--session', required=True)
    ask = sub.add_parser('xps-infer')
    ask.add_argument('--session', required=True)
    ask.add_argument('--model', required=True)
    ask.add_argument('--prompt', required=True)
    ask.add_argument('--authorize-inference', action='store_true')
    flow_start = sub.add_parser('workflow-begin')
    flow_start.add_argument('--state', required=True, type=Path)
    flow_start.add_argument('--run-id', required=True)
    flow_start.add_argument('--repo', required=True, type=Path)
    flow_start.add_argument('--session', required=True)
    flow_submit = sub.add_parser('workflow-submit')
    flow_submit.add_argument('--state', required=True, type=Path)
    flow_submit.add_argument('--run-id', required=True)
    flow_submit.add_argument(
        '--stage',
        required=True,
        choices=('lets-investigate', 'what-we-know', 'agents-assemble', 'stick-the-landing'),
    )
    flow_submit.add_argument('--evidence', required=True, type=Path)
    flow_submit.add_argument('--session', required=True)
    flow_approve = sub.add_parser('workflow-approve')
    flow_approve.add_argument('--state', required=True, type=Path)
    flow_approve.add_argument('--run-id', required=True)
    flow_approve.add_argument(
        '--stage',
        required=True,
        choices=('lets-investigate', 'what-we-know', 'agents-assemble', 'stick-the-landing'),
    )
    flow_approve.add_argument('--evidence-sha256', required=True)
    flow_approve.add_argument('--session', required=True)
    flow_approve.add_argument('--operator-confirmed', action='store_true')
    flow_status = sub.add_parser('workflow-status')
    flow_status.add_argument('--state', required=True, type=Path)
    flow_status.add_argument('--run-id', required=True)
    flow_status.add_argument('--session', required=True)
    sandbox = sub.add_parser('sandbox-tool')
    sandbox.add_argument('--repo', type=Path, required=True)
    sandbox.add_argument(
        '--tool', choices=('list-files', 'search-fixed', 'git-status'), required=True
    )
    sandbox.add_argument('--pattern', default='')
    sandbox.add_argument('--evidence', type=Path, required=True)
    sandbox.add_argument('--session', required=True)
    sandbox.add_argument('--authorize-execution', action='store_true')
    plan = sub.add_parser('plan')
    plan.add_argument('--file', required=True, type=Path)
    plan.add_argument('--session', required=True)
    assembly = sub.add_parser('assemble-live')
    assembly.add_argument('--repo', required=True, type=Path)
    assembly.add_argument('--plan', required=True, type=Path)
    assembly.add_argument('--state', required=True, type=Path)
    assembly.add_argument('--provider-registry', required=True, type=Path)
    assembly.add_argument('--session', required=True)
    assembly.add_argument('--authorize-inference', action='store_true')
    assembly.add_argument('--authorize-tools', action='store_true')
    assembly.add_argument('--xps-worker-model')
    assembly.add_argument('--xps-reviewer-model')
    apply_live = sub.add_parser('apply-live-assembly')
    apply_live.add_argument('--repo', required=True, type=Path)
    apply_live.add_argument('--receipt', required=True, type=Path)
    apply_live.add_argument('--workspace', required=True, type=Path)
    apply_live.add_argument('--session', required=True)
    apply_live.add_argument('--authorize-apply', action='store_true')
    live = sub.add_parser('live-review')
    live.add_argument('--repo', required=True, type=Path)
    live.add_argument('--state', required=True, type=Path)
    live.add_argument('--session', required=True)
    live.add_argument('--task-id', required=True)
    live.add_argument('--objective', required=True)
    live.add_argument('--owned-path', required=True)
    live.add_argument('--authorize-inference', action='store_true')
    apply = sub.add_parser('apply-reviewed')
    apply.add_argument('--repo', required=True, type=Path)
    apply.add_argument('--receipt', required=True, type=Path)
    apply.add_argument('--workspace', required=True, type=Path)
    apply.add_argument('--session', required=True)
    apply.add_argument('--authorize-apply', action='store_true')
    verify = sub.add_parser('verify-applied')
    verify.add_argument('--repo', required=True, type=Path)
    verify.add_argument('--receipt', required=True, type=Path)
    verify.add_argument('--workspace', required=True, type=Path)
    verify.add_argument('--evidence', required=True, type=Path)
    verify.add_argument('--session', required=True)
    verify.add_argument('--authorize-verify', action='store_true')
    checkpoint = sub.add_parser('checkpoint')
    checkpoint.add_argument('--receipt', required=True, type=Path)
    checkpoint.add_argument('--verification', required=True, type=Path)
    checkpoint.add_argument('--evidence', required=True, type=Path)
    checkpoint.add_argument('--session', required=True)
    checkpoint.add_argument('--authorize-record', action='store_true')
    functional = sub.add_parser('functional-verify')
    functional.add_argument('--checkpoint', required=True, type=Path)
    functional.add_argument('--workspace', required=True, type=Path)
    functional.add_argument('--evidence', required=True, type=Path)
    functional.add_argument('--session', required=True)
    functional.add_argument('--authorize-execution', action='store_true')
    repair_open = sub.add_parser('repair-open')
    repair_open.add_argument('--failure', required=True, type=Path)
    repair_open.add_argument('--checkpoint', required=True, type=Path)
    repair_open.add_argument('--state', required=True, type=Path)
    repair_open.add_argument('--session', required=True)
    repair_open.add_argument('--authorize-record', action='store_true')
    repair_reserve = sub.add_parser('repair-reserve')
    repair_reserve.add_argument('--state', required=True, type=Path)
    repair_reserve.add_argument('--task-id', required=True)
    repair_reserve.add_argument('--session', required=True)
    repair_reserve.add_argument('--authorize-reserve', action='store_true')
    repair_finish = sub.add_parser('repair-disposition')
    repair_finish.add_argument('--state', required=True, type=Path)
    repair_finish.add_argument('--task-id', required=True)
    repair_finish.add_argument(
        '--outcome', required=True, choices=('rejected', 'failed', 'passed', 'abandoned')
    )
    repair_finish.add_argument('--review', type=Path)
    repair_finish.add_argument('--verification', type=Path)
    repair_finish.add_argument('--session', required=True)
    repair_finish.add_argument('--authorize-record', action='store_true')
    repair_dispatch = sub.add_parser('repair-dispatch')
    repair_dispatch.add_argument('--state', required=True, type=Path)
    repair_dispatch.add_argument('--task-id', required=True)
    repair_dispatch.add_argument('--checkpoint', required=True, type=Path)
    repair_dispatch.add_argument('--objective', required=True)
    repair_dispatch.add_argument('--owned-path', action='append', required=True)
    repair_dispatch.add_argument('--session', required=True)
    repair_dispatch.add_argument('--authorize-inference', action='store_true')
    repair_dispatch.add_argument('--provider-registry', type=Path)
    repair_dispatch.add_argument('--gpu-policy', type=Path)
    repair_inspect = sub.add_parser('repair-inspect')
    repair_inspect.add_argument('--state', required=True, type=Path)
    repair_inspect.add_argument('--task-id', required=True)
    repair_inspect.add_argument('--session', required=True)
    inspect_cmd = sub.add_parser('inspect')
    inspect_cmd.add_argument('--state', required=True, type=Path)
    inspect_cmd.add_argument('--run-id', required=True)
    inspect_cmd.add_argument('--session', required=True)
    return parser


MAX_SESSION_ID_LENGTH = 128


def _session(value: str) -> str:
    if not value.strip() or len(value) > MAX_SESSION_ID_LENGTH or '\x00' in value:
        msg = 'Invalid Hermes session identifier'
        raise WorkflowError(msg)
    return value


def handle(args: argparse.Namespace) -> dict[str, object]:  # noqa: C901, PLR0911, PLR0912, PLR0915 - native bridge dispatch
    """Dispatch only explicitly authorized actions; never merge or execute generated code."""
    if args.command == 'doctor':
        return {
            'native_skills': 'instruction-only',
            'plan_validation': True,
            'durable_inspection': True,
            'local_model_adapters': 'opt-in-unqualified',
            'live_execution_qualified': False,
            'tool_sandbox_qualified': False,
            'human_grant_required': True,
        }
    session = _session(args.session)
    if args.command == 'xps-models':
        from hermes_harness.multiagent.dynamic_pool import available_models  # noqa: PLC0415

        return {'session_id': session, 'models': list(available_models()), 'mode': 'read-only'}
    if args.command == 'xps-infer':
        from hermes_harness.multiagent.dynamic_pool import infer  # noqa: PLC0415

        return {
            'session_id': session,
            **infer(
                args.model,
                args.prompt,
                authorized=args.authorize_inference,
            ),
        }
    if args.command in {'deploy-preflight', 'provider-probe', 'model-evaluate', 'gpu-switch'}:
        return handle_deployment(args)
    if args.command in {'workflow-begin', 'workflow-submit', 'workflow-approve', 'workflow-status'}:
        from hermes_harness.native.stage_approvals import (  # noqa: PLC0415
            approve,
            begin,
            snapshot,
            submit,
        )

        if args.command == 'workflow-begin':
            return begin(args.state, args.run_id, args.repo, session)
        if args.command == 'workflow-submit':
            return submit(args.state, args.run_id, args.stage, args.evidence, session)
        if args.command == 'workflow-approve':
            return approve(
                args.state,
                args.run_id,
                args.stage,
                args.evidence_sha256,
                session,
                operator_confirmed=args.operator_confirmed,
            )
        result = snapshot(args.state, args.run_id)
        if result.get('session') != session:
            msg = 'Originating Hermes session mismatch'
            raise WorkflowError(msg)
        return result
    if args.command == 'sandbox-tool':
        from hermes_harness.multiagent.sandbox_tools import (  # noqa: PLC0415
            record_sandbox_tool,
            run_sandbox_tool,
        )
        from hermes_harness.native.workflows import repository  # noqa: PLC0415

        sandbox_result = run_sandbox_tool(
            repository(args.repo),
            args.tool,
            pattern=args.pattern,
            authorized=args.authorize_execution,
        )
        digest = record_sandbox_tool(sandbox_result, args.evidence)
        return {
            'session_id': session,
            'tool': sandbox_result.tool,
            'exit_code': sandbox_result.exit_code,
            'stdout': sandbox_result.stdout,
            'stderr': sandbox_result.stderr,
            'sandbox': sandbox_result.sandbox,
            'host_execution': sandbox_result.host_execution,
            'evidence': str(args.evidence),
            'evidence_sha256': digest,
        }
    if args.command == 'assemble-live':
        from hermes_harness.native.live_assembly import live_assemble  # noqa: PLC0415

        return live_assemble(
            repo=args.repo,
            plan=args.plan,
            state=args.state,
            registry=args.provider_registry,
            session=session,
            authorize_inference=args.authorize_inference,
            authorize_tools=args.authorize_tools,
            xps_worker_model=args.xps_worker_model,
            xps_reviewer_model=args.xps_reviewer_model,
        )
    if args.command == 'apply-live-assembly':
        from hermes_harness.native.live_assembly import apply_live_assembly  # noqa: PLC0415

        return {
            'session_id': session,
            **apply_live_assembly(
                repo=args.repo,
                receipt=args.receipt,
                workspace=args.workspace,
                authorized=args.authorize_apply,
            ),
        }
    if args.command == 'live-review':
        from hermes_harness.native.live_review import reviewed_live_task  # noqa: PLC0415

        return reviewed_live_task(
            repo=args.repo,
            state=args.state,
            session=session,
            task_id=args.task_id,
            objective=args.objective,
            owned_path=args.owned_path,
            authorized=args.authorize_inference,
        )
    if args.command == 'apply-reviewed':
        from hermes_harness.native.worktree_apply import apply_reviewed_receipt  # noqa: PLC0415

        result = apply_reviewed_receipt(
            repo=args.repo,
            receipt=args.receipt,
            workspace=args.workspace,
            authorized=args.authorize_apply,
        )
        return {'session_id': session, **result}
    if args.command == 'verify-applied':
        from hermes_harness.native.verify_applied import verify_applied  # noqa: PLC0415

        result = verify_applied(
            repo=args.repo,
            receipt=args.receipt,
            workspace=args.workspace,
            evidence=args.evidence,
            authorized=args.authorize_verify,
        )
        return {'session_id': session, **result}
    if args.command == 'checkpoint':
        from hermes_harness.native.lifecycle import record_checkpoint  # noqa: PLC0415

        return {
            'session_id': session,
            **record_checkpoint(
                receipt=args.receipt,
                verification=args.verification,
                evidence=args.evidence,
                authorized=args.authorize_record,
            ),
        }
    if args.command == 'functional-verify':
        from hermes_harness.native.functional_verify import functional_verify  # noqa: PLC0415

        return {
            'session_id': session,
            **functional_verify(
                checkpoint=args.checkpoint,
                workspace=args.workspace,
                evidence=args.evidence,
                authorized=args.authorize_execution,
            ),
        }
    if args.command == 'repair-dispatch':
        from hermes_harness.native.repair_dispatch import dispatch_repair  # noqa: PLC0415

        return {
            'session_id': session,
            **dispatch_repair(
                state=args.state,
                task_id=args.task_id,
                checkpoint=args.checkpoint,
                objective=args.objective,
                owned_paths=tuple(args.owned_path),
                authorized=args.authorize_inference,
                registry_file=args.provider_registry,
                gpu_policy_file=args.gpu_policy,
            ),
        }
    if args.command in {'repair-open', 'repair-reserve', 'repair-inspect', 'repair-disposition'}:
        from hermes_harness.native.repair_controller import (  # noqa: PLC0415
            inspect_repair,
            open_repair,
            record_repair_disposition,
            reserve_repair,
        )

        if args.command == 'repair-open':
            result = open_repair(
                failure=args.failure,
                checkpoint=args.checkpoint,
                state=args.state,
                authorized=args.authorize_record,
            )
        elif args.command == 'repair-reserve':
            result = reserve_repair(
                state=args.state,
                task_id=args.task_id,
                authorized=args.authorize_reserve,
            )
        elif args.command == 'repair-disposition':
            result = record_repair_disposition(
                state=args.state,
                task_id=args.task_id,
                outcome=args.outcome,
                review=args.review,
                verification=args.verification,
                authorized=args.authorize_record,
            )
        else:
            from hermes_harness.native.repair_dispatch import inspect_dispatch  # noqa: PLC0415

            result = inspect_repair(state=args.state, task_id=args.task_id)
            result['dispatch'] = inspect_dispatch(state=args.state, task_id=args.task_id)
        return {'session_id': session, **result}
    if args.command == 'plan':
        path: Path = args.file
        if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_FILE_SIZE:
            msg = 'Missing, symlinked or oversized plan file'
            raise WorkflowError(msg)
        plan = parse_plan_json(path.read_text(encoding='utf-8'))
        return {
            'session_id': session,
            'mode': 'validation-only',
            'plan_id': plan.id,
            'ordered_nodes': [node.id for node in ordered_nodes(plan)],
            'roles': {node.id: node.role for node in plan.nodes},
            'dispatched': False,
        }
    if args.command == 'inspect':
        state: Path = args.state
        if state.is_symlink() or not state.is_dir():
            msg = 'State root must be an existing real directory'
            raise WorkflowError(msg)
        result = inspect(state, args.run_id)
        return {'session_id': session, 'mode': 'read-only', 'result': result}
    msg = 'Unknown bridge operation'
    raise WorkflowError(msg)


def main() -> None:
    args = _parser().parse_args()
    try:
        output = handle(args)
    except (WorkflowError, OSError, ValueError, KeyError, json.JSONDecodeError) as exc:
        print(json.dumps({'error': str(exc), 'ok': False}, sort_keys=True))
        raise SystemExit(2) from exc
    print(json.dumps({'ok': True, **output}, sort_keys=True))


if __name__ == '__main__':
    main()
