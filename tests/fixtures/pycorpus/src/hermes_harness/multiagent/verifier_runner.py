"""Bounded verifier process runner for disposable worktrees.

This executes an operator-selected command, NOT instructions from a model.
It is not an OS sandbox; use only with a separately restricted account or
container and never pass untrusted shell strings.
"""

import hashlib
import os
import shutil
import subprocess
from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.multiagent.runtime import WorkflowError
from hermes_harness.multiagent.verification_evidence import (
    TestEvidence,
    evaluate_test_evidence,
)
from hermes_harness.multiagent.verification_manifest import prepare_verification

if TYPE_CHECKING:
    from pathlib import Path

    from hermes_harness.multiagent.verification_evidence import (
        VerificationDecision,
    )

MAX_CAPTURE = 64 * 1024
MAX_VERIFICATION_SECONDS = 180
ALLOWED = {
    'pytest': ('uv', 'run', 'pytest', '-q'),
}


@dataclass(frozen=True, slots=True)
class VerificationRun:
    evidence: TestEvidence
    stdout: str
    stderr: str
    timed_out: bool


def run_verification(  # noqa: C901, PLR0913, PLR0917, PLR0915, PLR0914 - preserve audited boundary and established call contract
    workspace: Path,
    baseline: str,
    owned_paths: tuple[str, ...],
    verifier_identity: str,
    command_name: str,
    *,
    authorized: bool = False,
    trusted_code: bool = False,
    timeout_seconds: int = 60,
) -> VerificationRun:
    """Execute one allowlisted verifier and bind its result to file digests."""
    if not authorized:
        msg = 'Verification requires explicit trusted-caller authorization'
        raise WorkflowError(msg)
    if not trusted_code:
        msg = 'Untrusted code must use a qualified isolated verifier'
        raise WorkflowError(msg)
    if command_name not in ALLOWED:
        msg = 'Unknown verifier command'
        raise WorkflowError(msg)
    if not verifier_identity.strip() or not 1 <= timeout_seconds <= MAX_VERIFICATION_SECONDS:
        msg = 'Invalid verifier identity or timeout'
        raise WorkflowError(msg)
    if workspace.is_symlink() or not workspace.is_dir():
        msg = 'Verifier worktree must be an existing real directory'
        raise WorkflowError(msg)
    before = prepare_verification(workspace, baseline, owned_paths)
    command = ALLOWED[command_name]
    executable = shutil.which(command[0])
    if executable is None:
        msg = 'Verifier executable unavailable'
        raise WorkflowError(msg)
    safe_command = (executable, *command[1:])
    env = {
        'PATH': os.environ.get('PATH', '/usr/bin:/bin'),
        'HOME': str(workspace),
        'PYTHONDONTWRITEBYTECODE': '1',
        'PYTHONNOUSERSITE': '1',
        'GIT_TERMINAL_PROMPT': '0',
        'UV_OFFLINE': '1',
        'PIP_NO_INDEX': '1',
        'PYTEST_DISABLE_PLUGIN_AUTOLOAD': '1',
    }
    timed_out = False
    try:
        completed = subprocess.run(  # noqa: S603 - fixed command allowlist
            safe_command,
            cwd=workspace,
            env=env,
            capture_output=True,
            timeout=timeout_seconds,
            check=False,
            shell=False,
        )
        returncode = completed.returncode
        out = completed.stdout[:MAX_CAPTURE]
        err = completed.stderr[:MAX_CAPTURE]
    except subprocess.TimeoutExpired as exc:
        timed_out = True
        returncode = 124
        out = (exc.stdout or b'')[:MAX_CAPTURE]
        err = (exc.stderr or b'')[:MAX_CAPTURE]
    after = prepare_verification(workspace, baseline, owned_paths)
    if after != before:
        msg = 'Verifier mutated the audited integration snapshot'
        raise WorkflowError(msg)
    output_hash = hashlib.sha256(out + b'\0' + err).hexdigest()
    evidence = TestEvidence(
        before.baseline,
        before.head,
        before.changed_paths,
        before.file_digests,
        verifier_identity,
        returncode,
        output_hash,
    )
    return VerificationRun(
        evidence,
        out.decode('utf-8', errors='replace'),
        err.decode('utf-8', errors='replace'),
        timed_out,
    )


def verify_and_evaluate(  # noqa: PLR0913, PLR0917 - preserve audited boundary and established call contract
    workspace: Path,
    baseline: str,
    owned_paths: tuple[str, ...],
    verifier_identity: str,
    *,
    authorized: bool = False,
    trusted_code: bool = False,
    timeout_seconds: int = 60,
) -> tuple[VerificationRun, VerificationDecision]:
    """Run allowlisted pytest and evaluate snapshot-bound evidence without merging."""
    if not authorized:
        msg = 'Explicit verification authorization required'
        raise WorkflowError(msg)
    if not trusted_code:
        msg = 'Untrusted code must use a qualified isolated verifier'
        raise WorkflowError(msg)
    manifest = prepare_verification(workspace, baseline, owned_paths)
    result = run_verification(
        workspace,
        baseline,
        owned_paths,
        verifier_identity,
        'pytest',
        authorized=True,
        trusted_code=trusted_code,
        timeout_seconds=timeout_seconds,
    )
    decision = evaluate_test_evidence(manifest, result.evidence)
    return result, decision
