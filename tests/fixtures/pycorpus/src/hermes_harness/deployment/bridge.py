"""Internal Hermes skill bridge for explicit deployment qualification operations."""

import socket
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.deployment.evaluation import evaluate_samples, load_samples
from hermes_harness.deployment.gpu import (
    UserSystemd,
    load_gpu_policy,
    observed_free_vram_mib,
    switch_gpu_slot,
)
from hermes_harness.deployment.preflight import hp_preflight
from hermes_harness.deployment.providers import load_registry, probe_provider
from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.repair_controller import _write_once

if TYPE_CHECKING:
    import argparse


def add_deployment_commands(sub: argparse._SubParsersAction[argparse.ArgumentParser]) -> None:  # noqa: PLR0915
    """Register internal bridge commands; Hermes skills remain the only user UX."""
    doctor = sub.add_parser('deploy-preflight')
    doctor.add_argument('--workspace', type=Path, required=True)
    doctor.add_argument('--provider-registry', type=Path)
    doctor.add_argument('--gpu-policy', type=Path)
    doctor.add_argument('--gateway-unit', default='')
    doctor.add_argument('--probe-network', action='store_true')
    doctor.add_argument('--session', required=True)

    probe = sub.add_parser('provider-probe')
    probe.add_argument('--provider-registry', type=Path, required=True)
    probe.add_argument('--role', required=True)
    probe.add_argument('--session', required=True)

    eval_cmd = sub.add_parser('model-evaluate')
    eval_cmd.add_argument('--sample-file', type=Path, required=True)
    eval_cmd.add_argument('--evidence-root', type=Path, required=True)
    eval_cmd.add_argument('--output', type=Path)
    eval_cmd.add_argument('--authorize-record', action='store_true')
    eval_cmd.add_argument('--session', required=True)

    switch = sub.add_parser('gpu-switch')
    switch.add_argument('--gpu-policy', type=Path, required=True)
    switch.add_argument('--provider-registry', type=Path, required=True)
    switch.add_argument('--prior-unit', required=True)
    switch.add_argument('--target-unit', required=True)
    switch.add_argument('--transaction', required=True)
    switch.add_argument('--journal', type=Path, required=True)
    switch.add_argument('--authorize-switch', action='store_true')
    switch.add_argument('--session', required=True)


def handle_deployment(args: argparse.Namespace) -> dict[str, object]:  # noqa: C901, PLR0912, PLR0915
    """Only the explicit gpu-switch command can mutate host service state."""
    session = args.session
    if args.command == 'deploy-preflight':
        result = hp_preflight(
            workspace=args.workspace,
            registry=load_registry(args.provider_registry) if args.provider_registry else None,
            gpu=load_gpu_policy(args.gpu_policy) if args.gpu_policy else None,
            probe_network=args.probe_network,
            gateway_unit=args.gateway_unit,
        )
    elif args.command == 'provider-probe':
        registry = load_registry(args.provider_registry)
        result = probe_provider(registry.role(args.role))
    elif args.command == 'model-evaluate':
        result = evaluate_samples(load_samples(args.sample_file, args.evidence_root))
        if args.output is not None:
            if not args.authorize_record:
                msg = 'Evaluation recording requires separate authorization'
                raise WorkflowError(msg)
            if args.output.parent.resolve() != args.evidence_root.resolve():
                msg = 'Evaluation output must remain inside evidence root'
                raise WorkflowError(msg)
            digest = _write_once(args.output, result)
            result = {**result, 'evidence': str(args.output), 'evidence_sha256': digest}
    elif args.command == 'gpu-switch':
        policy = load_gpu_policy(args.gpu_policy)
        if socket.gethostname() != policy.host:
            msg = 'GPU service switching only permitted on owning host'
            raise WorkflowError(msg)
        registry = load_registry(args.provider_registry)
        matched = [
            spec
            for spec in registry.profiles.values()
            if spec.unit == args.target_unit and spec.gpu_host == policy.host
        ]
        if len(matched) != 1:
            msg = 'GPU target has no unique registered provider identity'
            raise WorkflowError(msg)
        result = switch_gpu_slot(
            policy=policy,
            prior=args.prior_unit,
            target=args.target_unit,
            transaction=args.transaction,
            journal=args.journal,
            systemd=UserSystemd(),
            ready=lambda unit: bool(
                unit.model == matched[0].model
                and probe_provider(matched[0])['model_identity_verified']
            ),
            authorized=args.authorize_switch,
            free_vram_mib=observed_free_vram_mib,
        )
    else:
        msg = 'Unknown deployment action'
        raise WorkflowError(msg)
    return {'session_id': session, **result}
