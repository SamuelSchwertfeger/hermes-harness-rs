"""Conservative, operator-authorized GPU slot leasing and systemd switching.

A host must share one flock path among ALL harness inference and switch callers.
Unmanaged and pinned units are never stopped. A crash leaves an immutable claim
whose outcome remains UNKNOWN; it is never implicitly retried.
"""

import fcntl
import json
import os
import subprocess
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Protocol

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.native.repair_controller import _write_once

if TYPE_CHECKING:
    from collections.abc import Callable, Iterator

MAX_POLICY_SIZE = 32_768


@dataclass(frozen=True, slots=True)
class GPUUnit:
    name: str
    vram_mib: int
    pinned: bool
    managed: bool
    model: str


@dataclass(frozen=True, slots=True)
class GPUPolicy:
    host: str
    capacity_mib: int
    reserved_mib: int
    lock_path: Path
    units: tuple[GPUUnit, ...]

    def validate(self) -> None:
        if not self.host or self.capacity_mib <= 0 or self.reserved_mib < 0:
            msg = 'Invalid GPU policy capacities'
            raise WorkflowError(msg)
        if self.reserved_mib >= self.capacity_mib:
            msg = 'GPU reserve exceeds physical capacity'
            raise WorkflowError(msg)
        if self.lock_path.is_symlink() or not self.lock_path.parent.is_dir():
            msg = 'GPU lock directory unavailable'
            raise WorkflowError(msg)
        if len({x.name for x in self.units}) != len(self.units):
            msg = 'Duplicate GPU service identity'
            raise WorkflowError(msg)
        for unit in self.units:
            if not unit.name.endswith('.service') or '/' in unit.name or unit.vram_mib <= 0:
                msg = 'Invalid GPU unit or memory estimate'
                raise WorkflowError(msg)
        pinned_total = sum(u.vram_mib for u in self.units if u.pinned)
        if pinned_total + self.reserved_mib > self.capacity_mib:
            msg = 'Pinned services exceed GPU budget'
            raise WorkflowError(msg)

    def unit(self, name: str) -> GPUUnit:
        for unit in self.units:
            if unit.name == name:
                return unit
        msg = 'GPU service not in operator policy'
        raise WorkflowError(msg)

    def admit(self, names: tuple[str, ...]) -> None:
        self.validate()
        selected = {self.unit(name).name for name in names}
        selected.update(unit.name for unit in self.units if unit.pinned)
        budget = self.reserved_mib + sum(self.unit(name).vram_mib for name in selected)
        if budget > self.capacity_mib:
            msg = 'GPU VRAM reservation would overcommit'
            raise WorkflowError(msg)


def load_gpu_policy(path: Path) -> GPUPolicy:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_POLICY_SIZE:
        msg = 'Invalid GPU policy file'
        raise WorkflowError(msg)
    try:
        data = json.loads(path.read_text())
        units = tuple(GPUUnit(**entry) for entry in data['units'])
        policy = GPUPolicy(
            data['host'], data['capacity_mib'], data['reserved_mib'], Path(data['lock_path']), units
        )
        policy.validate()
    except (OSError, ValueError, TypeError, KeyError) as exc:
        msg = 'Malformed GPU policy'
        raise WorkflowError(msg) from exc
    return policy


@contextmanager
def gpu_lane(policy: GPUPolicy) -> Iterator[None]:
    """One nonblocking cross-process lease. Never clears an existing failure journal."""
    policy.validate()
    fd = os.open(policy.lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            msg = 'GPU lane busy; refuse overlapping dispatch or switch'
            raise WorkflowError(msg) from exc
        yield
    finally:
        os.close(fd)


class Systemd(Protocol):
    def active(self, unit: str) -> bool: ...
    def start(self, unit: str) -> None: ...
    def stop(self, unit: str) -> None: ...


class UserSystemd:
    """No sudo or shell, no enable/disable. Requires preconfigured authorization."""

    def _run(self, operation: str, unit: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(  # noqa: S603
            ['/usr/bin/systemctl', operation, unit],
            capture_output=True,
            text=True,
            check=False,
            timeout=25,
        )

    def active(self, unit: str) -> bool:
        return self._run('is-active', unit).returncode == 0

    def start(self, unit: str) -> None:
        if self._run('start', unit).returncode != 0:
            msg = 'Approved GPU service start failed'
            raise WorkflowError(msg)

    def stop(self, unit: str) -> None:
        if self._run('stop', unit).returncode != 0:
            msg = 'Approved GPU service stop failed'
            raise WorkflowError(msg)


def switch_gpu_slot(  # noqa: C901, PLR0912, PLR0913, PLR0915
    *,
    policy: GPUPolicy,
    prior: str,
    target: str,
    transaction: str,
    journal: Path,
    systemd: Systemd,
    ready: Callable[[GPUUnit], bool],
    authorized: bool = False,
    free_vram_mib: Callable[[], int] | None = None,
) -> dict[str, object]:
    """One-shot conservative switch; failures require operator reconciliation.

    The ready callback must validate health, model identity and authenticated chat.
    There is no automatic rollback if a service state is uncertain.
    """
    if not authorized:
        msg = 'Explicit host model-switch authorization required'
        raise WorkflowError(msg)
    old = policy.unit(prior)
    new = policy.unit(target)
    if old.pinned or not old.managed or not new.managed:
        msg = 'Pinned or unmanaged service may not be switched'
        raise WorkflowError(msg)
    if old.name == new.name:
        msg = 'No-op GPU switch not permitted'
        raise WorkflowError(msg)
    policy.admit((target,))
    if not journal.is_dir() or journal.is_symlink():
        msg = 'Invalid GPU switch journal'
        raise WorkflowError(msg)
    if not transaction or any(
        c not in 'abcdefghijklmnopqrstuvwxyz0123456789-' for c in transaction
    ):
        msg = 'Invalid transaction identifier'
        raise WorkflowError(msg)
    record = {
        'mode': 'gpu-switch-claim',
        'host': policy.host,
        'prior': prior,
        'target': target,
        'transaction': transaction,
        'phase': 'UNKNOWN',
        'operator_authorized': True,
    }
    with gpu_lane(policy):
        # Before a claim, demand an unambiguous starting state.
        if not systemd.active(prior) or systemd.active(target):
            msg = 'Unqualified initial GPU service state'
            raise WorkflowError(msg)
        claim_file = journal / f'{transaction}-claim.json'
        try:
            claim_sha = _write_once(claim_file, record)
        except FileExistsError as exc:
            msg = 'GPU transaction already claimed; do not replay'
            raise WorkflowError(msg) from exc
        try:
            systemd.stop(prior)
            if systemd.active(prior):
                msg = 'Prior GPU service did not drain'
                raise WorkflowError(msg)  # noqa: TRY301
            if free_vram_mib is not None and free_vram_mib() < new.vram_mib + policy.reserved_mib:
                msg = 'Insufficient observed GPU free memory for switch'
                raise WorkflowError(msg)  # noqa: TRY301
            systemd.start(target)
            if not systemd.active(target) or not ready(new):
                msg = 'Target GPU model not authenticated and ready'
                raise WorkflowError(msg)  # noqa: TRY301
            outcome = 'READY'
        except (WorkflowError, OSError, subprocess.TimeoutExpired, ValueError, TypeError) as exc:
            # The original process may have stopped; do not retry/rollback blindly.
            outcome = 'UNKNOWN'
            _write_once(
                journal / f'{transaction}-result.json',
                {
                    'mode': 'gpu-switch-result',
                    'claim_sha256': claim_sha,
                    'phase': outcome,
                    'manual_reconciliation_required': True,
                },
            )
            msg = 'GPU switch outcome UNKNOWN; operator recovery required'
            raise WorkflowError(msg) from exc
        result = {
            'mode': 'gpu-switch-result',
            'claim_sha256': claim_sha,
            'phase': outcome,
            'manual_reconciliation_required': False,
        }
        _write_once(journal / f'{transaction}-result.json', result)
        return result


def observed_free_vram_mib() -> int:
    """Read observed free memory on an NVIDIA one-GPU host; never infer free VRAM."""
    response = subprocess.run(
        ['/usr/bin/nvidia-smi', '--query-gpu=memory.free', '--format=csv,noheader,nounits'],
        capture_output=True,
        text=True,
        check=False,
        timeout=8,
    )
    if response.returncode != 0 or len(response.stdout.splitlines()) != 1:
        msg = 'NVIDIA GPU free-memory observation unavailable'
        raise WorkflowError(msg)
    try:
        value = int(response.stdout.strip())
    except ValueError as exc:
        msg = 'Invalid NVIDIA VRAM observation'
        raise WorkflowError(msg) from exc
    if value <= 0:
        msg = 'Invalid NVIDIA VRAM observation'
        raise WorkflowError(msg)
    return value
