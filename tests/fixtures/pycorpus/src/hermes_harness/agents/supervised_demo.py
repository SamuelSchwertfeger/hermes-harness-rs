"""Opt-in OFFLINE demonstration with disposable Git and no network gateway."""

import argparse
import asyncio
import json
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING
from uuid import uuid4

from hermes_harness.agents.supervised_agents import SupervisedPolicy
from hermes_harness.agents.supervised_execution import ScriptedExecution, execute_scripted_once
from hermes_harness.core.delegation import Brief, Finding
from hermes_harness.gateway.gateway_identity import Submission
from hermes_harness.gateway.scripted_supervised_gateway import ScriptedSupervisedGateway
from hermes_harness.git.worktree_delegation import TaskWorkspace

if TYPE_CHECKING:
    from hermes_harness.agents.supervised_reviewer_dispatch import RuntimeBinding


@dataclass(slots=True)
class ScriptedOnlyQualifier:
    """Not a live sandbox attestation; only valid with built-in scripted gateway."""

    def require_qualified(self, binding: RuntimeBinding) -> None:
        if not binding.workspace.is_dir() or binding.workspace.is_symlink():
            msg = 'Scripted workspace is not a real directory'
            raise PermissionError(msg)


@dataclass(slots=True)
class DemoWorker:
    workspace: Path

    def run(self, brief: Brief, prior: Finding | None) -> Finding:
        if prior is not None:
            msg = 'Worker cannot receive prior evidence'
            raise ValueError(msg)
        target = self.workspace / 'demo-result.txt'
        target.write_text('offline scripted worker output\n')
        return Finding(brief.worker_id, 'Demo worker completed')


class DemoReviewer:
    """Separate non-provider reviewer double."""

    def run(self, brief: Brief, prior: Finding | None) -> Finding:
        if prior is None or prior.agent_id != brief.worker_id:
            msg = 'Missing worker evidence'
            raise ValueError(msg)
        return Finding(brief.reviewer_id, 'Scripted review only: request human approval')


def _git(repo: Path, *args: str) -> None:
    subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True)  # noqa: S603, S607


def main() -> None:
    """Create ONE disposable repository and demonstrate the scripted flow."""
    parser = argparse.ArgumentParser(description='OFFLINE ONLY supervised gateway demonstration')
    parser.add_argument('--root', type=Path, required=True)
    args = parser.parse_args()
    root: Path = args.root.resolve()
    if root.exists():
        parser.error('Root must not exist; old evidence is never overwritten')
    root.mkdir(mode=0o700, parents=True)
    repo = root / 'repo'
    repo.mkdir()
    _git(repo, 'init')
    _git(repo, 'config', 'user.email', 'offline@example.invalid')
    _git(repo, 'config', 'user.name', 'Offline qualification')
    (repo / 'README.md').write_text('disposable baseline\n')
    _git(repo, 'add', 'README.md')
    _git(repo, 'commit', '-m', 'baseline')
    state = root / 'state'
    state.mkdir()
    workspace = root / 'workspace'
    brief = Brief(
        'scripted-demo',
        'Scripted worker reviewer integration',
        ('demo-result.txt',),
        'worker',
        'reviewer',
    )
    config = ScriptedExecution(
        task=TaskWorkspace(state, workspace, brief),
        source_repo=repo,
        policy=SupervisedPolicy(authorized_task_id=brief.task_id, authorized_workspace=workspace),
        worker_submission=Submission('worker-scripted', uuid4().hex),
        reviewer_submission=Submission('reviewer-scripted', uuid4().hex),
        ledger_path=root / 'admissions.sqlite',
        rpc=ScriptedSupervisedGateway(),
        qualifier=ScriptedOnlyQualifier(),
        worker=DemoWorker(workspace),
        reviewer=DemoReviewer(),
    )
    result = asyncio.run(execute_scripted_once(config))
    print(
        json.dumps(
            {
                'mode': 'OFFLINE SCRIPTED (NO HERMES)',
                'worker_admission': result.worker_admission_id,
                'reviewer_admission': result.reviewer_admission_id,
                'audit_sha256': result.audit_sha256,
                'approval_request': str(result.approval_request),
                'approved': False,
            },
            indent=2,
        )
    )


if __name__ == '__main__':
    main()

# Temporary exports for legacy module import compatibility.
__all__ = [
    'Brief',
    'DemoReviewer',
    'DemoWorker',
    'Finding',
    'Path',
    'ScriptedExecution',
    'ScriptedOnlyQualifier',
    'ScriptedSupervisedGateway',
    'Submission',
    'SupervisedPolicy',
    'TaskWorkspace',
    '_git',
    'argparse',
    'asyncio',
    'dataclass',
    'execute_scripted_once',
    'json',
    'main',
    'subprocess',
    'uuid4',
]
