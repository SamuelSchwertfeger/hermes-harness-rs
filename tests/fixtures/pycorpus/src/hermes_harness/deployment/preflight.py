"""Read-only HP deployment readiness report; never modifies gateway/host settings."""

import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import TYPE_CHECKING

from hermes_harness.deployment.gpu import gpu_lane
from hermes_harness.deployment.providers import ProviderError, probe_provider

if TYPE_CHECKING:
    from hermes_harness.deployment.gpu import GPUPolicy
    from hermes_harness.deployment.providers import ProviderRegistry


def hp_preflight(  # noqa: C901, PLR0912, PLR0913, PLR0915
    *,
    workspace: Path,
    registry: ProviderRegistry | None = None,
    gpu: GPUPolicy | None = None,
    probe_network: bool = False,
    gateway_unit: str = '',
) -> dict[str, object]:
    """No files written, no host model switches. Probe network only on opt-in."""
    checks: dict[str, dict[str, object]] = {}

    def record(name: str, detail: str, *, ok: bool, optional: bool = False) -> None:  # noqa: PLR0913
        checks[name] = {
            'status': 'PASS' if ok else 'WARN' if optional else 'BLOCK',
            'detail': detail,
        }

    record('python', 'Python >=3.14 required', ok=sys.version_info >= (3, 14))
    record('uv', 'uv must be on service PATH', ok=bool(shutil.which('uv')))
    record('bubblewrap', 'Bubblewrap required for functional tests', ok=bool(shutil.which('bwrap')))
    record(
        'workspace',
        'Existing hermes-owned disposable workspace required',
        ok=workspace.is_dir()
        and not workspace.is_symlink()
        and os.access(workspace, os.R_OK | os.W_OK),
    )
    record(
        'identity',
        'Run as dedicated non-root Hermes identity',
        ok=Path.home() != Path('/home/operator') and os.geteuid() != 0,
    )
    if gateway_unit:
        if not gateway_unit.endswith('.service') or '/' in gateway_unit:
            record('gateway', 'Invalid gateway unit name', ok=False)
        else:
            response = subprocess.run(  # noqa: S603
                ['/usr/bin/systemctl', 'show', gateway_unit, '--property=ActiveState'],
                capture_output=True,
                text=True,
                timeout=8,
                check=False,
            )
            record(
                'gateway',
                'Existing gateway active state (no restart)',
                ok=response.returncode == 0 and 'ActiveState=active' in response.stdout,
            )
    else:
        record('gateway', 'Gateway unit not supplied', ok=False, optional=True)
    if registry is None:  # noqa: PLR1702
        record('provider', 'No operator registry configured', ok=False)
    else:
        try:
            for role in registry.roles:
                registry.role(role)
            record('provider', 'Role profiles validated (not yet live qualified)', ok=True)
            if probe_network:
                for name, spec in registry.profiles.items():
                    try:
                        result = probe_provider(spec)
                        record(
                            f'endpoint:{name}',
                            'Authenticated identity probe passed; 64K/tool reliability unverified',
                            ok=bool(result['model_identity_verified']),
                        )
                    except ProviderError as exc:
                        record(f'endpoint:{name}', str(exc), ok=False)
        except ProviderError as exc:
            record('provider', str(exc), ok=False)
    if gpu is None:
        record('gpu_lane', 'GPU policy missing', ok=False, optional=True)
    else:
        try:
            with gpu_lane(gpu):
                gpu.admit(())
            record('gpu_lane', 'Lease acquired and pinned estimate admitted', ok=True)
        except (OSError, ValueError, RuntimeError) as exc:
            record('gpu_lane', type(exc).__name__, ok=False)
    # This is a read-only backend preflight, not a real Telegram/skill qualification.
    record('native_session', 'Real Hermes gateway skill receipt not supplied', ok=False)
    record('context_64k', 'Long-prompt/token-usage qualification not supplied', ok=False)
    record('tool_qualification', 'Actual tool-execution receipts not supplied', ok=False)
    return {
        'mode': 'readonly-preflight',
        'checks': checks,
        'ready': all(item['status'] != 'BLOCK' for item in checks.values()),
        'host_mutated': False,
        'model_switched': False,
        'gateway_modified': False,
        'generated_code_executed': False,
    }
