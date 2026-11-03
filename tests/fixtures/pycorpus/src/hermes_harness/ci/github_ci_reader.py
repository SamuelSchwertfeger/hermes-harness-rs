"""Read-only GitHub Actions REST observation retrieval for offline CI candidates.

Only an operator-controlled, authenticated transport can provide trustworthy origin.
A returned observation is not an attestation, gate, or approval decision.
"""

import json
import logging
import re
from dataclasses import dataclass
from http import HTTPStatus
from typing import TYPE_CHECKING, Protocol
from urllib.error import HTTPError, URLError
from urllib.request import HTTPRedirectHandler, Request, build_opener

from hermes_harness.ci.ci_observation import (
    GitHubEvidence,
    GitHubJobObservation,
    GitHubRunObservation,
)
from hermes_harness.core.delegation import DelegationError

if TYPE_CHECKING:
    from hermes_harness.ci.ci_observation import CIPolicy
    from hermes_harness.ci.ci_request import CIRequest

_LOG = logging.getLogger(__name__)
_API = 'https://api.github.com'
_MAX_BYTES = 2_000_000
_MAX_PAGES = 20
_PAGE_SIZE = 100
_MAX_TIMEOUT = 30
_REPOSITORY = re.compile(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+\Z')
_SHA = re.compile(r'[0-9a-f]{40}\Z')
_ALLOWED_PATH = re.compile(
    r'/repos/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/'
    r'(?:actions/runs/[0-9]+(?:/attempts/[0-9]+/jobs\?per_page=100&page=[0-9]+)?'
    r'|git/commits/[0-9a-f]{40})\Z'
)


class JSONReader(Protocol):
    """Injection seam; implement with authenticated HTTPS for real observations."""

    def __call__(self, path: str) -> dict[str, object]: ...


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(
        self,
        _req: Request,
        _fp: object,
        _code: int,
        _msg: str,
        _headers: object,
        _newurl: str,
    ) -> None:
        """Refuse redirects rather than forwarding a bearer token to a new host."""
        return


@dataclass(frozen=True, slots=True)
class GitHubReadClient:
    """GET-only fixed-origin client; token never enters URLs or logs."""

    token: str
    timeout_seconds: float = 10.0

    def __call__(self, path: str) -> dict[str, object]:
        if not self.token or not (0 < self.timeout_seconds <= _MAX_TIMEOUT):
            msg = 'GitHub reader needs a token and bounded timeout'
            raise DelegationError(msg)
        if not _ALLOWED_PATH.fullmatch(path):
            msg = 'GitHub REST path is outside supported read-only scope'
            raise DelegationError(msg)
        # The URL is anchored at a constant HTTPS API origin and the relative
        # path has passed the allowlist above; other schemes are impossible.
        req = Request(  # noqa: S310 - fixed HTTPS origin and allowlisted path
            _API + path,
            headers={
                'Authorization': f'Bearer {self.token}',
                'Accept': 'application/vnd.github+json',
                'X-GitHub-Api-Version': '2022-11-28',
                'User-Agent': 'Hermes-Harness-ReadOnly',
            },
            method='GET',
        )
        try:
            with build_opener(_NoRedirect()).open(req, timeout=self.timeout_seconds) as response:
                if response.status != HTTPStatus.OK:
                    msg = 'GitHub returned an unexpected status'
                    raise DelegationError(msg)
                data = response.read(_MAX_BYTES + 1)
                if len(data) > _MAX_BYTES:
                    msg = 'GitHub response exceeds size bound'
                    raise DelegationError(msg)
        except (HTTPError, URLError, TimeoutError, OSError) as exc:
            _LOG.warning('GitHub read failed for a bounded API path')
            msg = 'GitHub API read failed; CI state is unknown'
            raise DelegationError(msg) from exc
        try:
            decoded = json.loads(data)
        except (ValueError, UnicodeError) as exc:
            msg = 'GitHub API returned malformed JSON'
            raise DelegationError(msg) from exc
        return _object(decoded)


def _object(value: object) -> dict[str, object]:
    if not isinstance(value, dict) or not all(isinstance(k, str) for k in value):
        msg = 'Expected JSON object from GitHub'
        raise DelegationError(msg)
    return value


def _string(value: object) -> str:
    if not isinstance(value, str):
        msg = 'Invalid GitHub string field'
        raise DelegationError(msg)
    return value


def _positive(value: object) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        msg = 'Invalid GitHub numeric identifier'
        raise DelegationError(msg)
    return value


def _conclusion(value: object) -> str | None:
    if value is None:
        return None
    return _string(value)


def _validate_request(candidate: CIRequest, policy: CIPolicy, run_id: int) -> str:
    if (
        not _REPOSITORY.fullmatch(policy.repository)
        or any(part in ('.', '..') for part in policy.repository.split('/'))
        or not _SHA.fullmatch(candidate.commit)
    ):
        msg = 'Invalid requested repository or commit'
        raise DelegationError(msg)
    if not policy.workflow_path.startswith('.github/workflows/') or '..' in policy.workflow_path:
        msg = 'Invalid requested workflow path'
        raise DelegationError(msg)
    _positive(run_id)
    return f'/repos/{policy.repository}'


def _workflow_path(raw: object, policy: CIPolicy) -> str:
    path = _string(raw)
    # GitHub returns e.g. owner/repo/.github/workflows/ci.yml@refs/heads/main.
    prefix = f'{policy.repository}/{policy.workflow_path}@'
    if not path.startswith(prefix) or len(path) <= len(prefix):
        msg = 'Workflow run does not match the requested workflow'
        raise DelegationError(msg)
    return policy.workflow_path


@dataclass(frozen=True, slots=True)
class _ReadContext:
    base: str
    run_id: int
    policy: CIPolicy
    candidate: CIRequest


def _read_run(fetch: JSONReader, context: _ReadContext) -> GitHubRunObservation:
    payload = _object(fetch(f'{context.base}/actions/runs/{context.run_id}'))
    repo = _object(payload.get('repository'))
    repository = _string(repo.get('full_name'))
    workflow = _workflow_path(payload.get('path'), context.policy)
    head_sha = _string(payload.get('head_sha'))
    actual_id = _positive(payload.get('id'))
    attempt = _positive(payload.get('run_attempt'))
    if (
        repository != context.policy.repository
        or head_sha != context.candidate.commit
        or actual_id != context.run_id
    ):
        msg = 'GitHub run identity does not match candidate'
        raise DelegationError(msg)
    return GitHubRunObservation(
        repository,
        workflow,
        head_sha,
        _string(payload.get('event')),
        actual_id,
        attempt,
        _string(payload.get('status')),
        _conclusion(payload.get('conclusion')),
    )


def _verify_tree(fetch: JSONReader, base: str, candidate: CIRequest) -> None:
    commit = _object(fetch(f'{base}/git/commits/{candidate.commit}'))
    tree = _object(commit.get('tree'))
    if _string(commit.get('sha')) != candidate.commit or _string(tree.get('sha')) != candidate.tree:
        msg = 'GitHub commit tree does not match immutable CI candidate'
        raise DelegationError(msg)


def _job_total(value: object) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        msg = 'Unbounded or malformed GitHub job count'
        raise DelegationError(msg)
    if not 0 <= value <= _PAGE_SIZE * _MAX_PAGES:
        msg = 'Unbounded or malformed GitHub job count'
        raise DelegationError(msg)
    return value


def _job_batch(payload: dict[str, object], total: int, prior: int) -> list[object]:
    if _job_total(payload.get('total_count')) != total:
        msg = 'GitHub job pagination changed or is invalid'
        raise DelegationError(msg)
    batch = payload.get('jobs')
    if not isinstance(batch, list):
        msg = 'GitHub job pagination changed or is invalid'
        raise DelegationError(msg)
    if len(batch) > _PAGE_SIZE or prior + len(batch) > total:
        msg = 'GitHub job pagination exceeds reported count'
        raise DelegationError(msg)
    return batch


def _parse_job(
    entry: object,
    run: GitHubRunObservation,
    seen_ids: set[int],
) -> GitHubJobObservation:
    job = _object(entry)
    job_id = _positive(job.get('id'))
    if job_id in seen_ids or _positive(job.get('run_id')) != run.run_id:
        msg = 'Duplicate or mismatched GitHub job identity'
        raise DelegationError(msg)
    if 'run_attempt' in job and job['run_attempt'] != run.run_attempt:
        msg = 'Duplicate or mismatched GitHub job identity'
        raise DelegationError(msg)
    seen_ids.add(job_id)
    return GitHubJobObservation(
        run.run_id,
        run.run_attempt,
        _string(job.get('name')),
        _string(job.get('status')),
        _conclusion(job.get('conclusion')),
    )


def _read_jobs(
    fetch: JSONReader,
    base: str,
    run: GitHubRunObservation,
) -> tuple[GitHubJobObservation, ...]:
    jobs: list[GitHubJobObservation] = []
    seen_ids: set[int] = set()
    total: int | None = None
    for page in range(1, _MAX_PAGES + 1):
        path = (
            f'{base}/actions/runs/{run.run_id}/attempts/{run.run_attempt}'
            f'/jobs?per_page={_PAGE_SIZE}&page={page}'
        )
        payload = _object(fetch(path))
        if total is None:
            total = _job_total(payload.get('total_count'))
        batch = _job_batch(payload, total, len(jobs))
        jobs.extend(_parse_job(entry, run, seen_ids) for entry in batch)
        if len(jobs) == total:
            return tuple(jobs)
        if not batch:
            break
    msg = 'GitHub job pagination incomplete; no CI conclusion can be inferred'
    raise DelegationError(msg)


def fetch_github_ci_evidence(  # noqa: PLR0913 - stable public API with injected reader
    candidate: CIRequest,
    policy: CIPolicy,
    run_id: int,
    *,
    reader: JSONReader,
) -> GitHubEvidence:
    """Fetch exact run, Git commit tree, and all jobs of its latest attempt.

    A caller-injected reader is for deterministic tests; injection alone supplies
    no provenance. Do not grant approval based on these returned values.
    """
    base = _validate_request(candidate, policy, run_id)
    run = _read_run(reader, _ReadContext(base, run_id, policy, candidate))
    _verify_tree(reader, base, candidate)
    return GitHubEvidence(run, _read_jobs(reader, base, run))


# Temporary exports for legacy module import compatibility.
__all__ = [
    '_ALLOWED_PATH',
    '_API',
    '_LOG',
    '_MAX_BYTES',
    '_MAX_PAGES',
    '_MAX_TIMEOUT',
    '_PAGE_SIZE',
    '_REPOSITORY',
    '_SHA',
    'DelegationError',
    'GitHubEvidence',
    'GitHubJobObservation',
    'GitHubReadClient',
    'GitHubRunObservation',
    'HTTPError',
    'HTTPRedirectHandler',
    'HTTPStatus',
    'JSONReader',
    'Protocol',
    'Request',
    'URLError',
    '_NoRedirect',
    '_ReadContext',
    '_conclusion',
    '_job_batch',
    '_job_total',
    '_object',
    '_parse_job',
    '_positive',
    '_read_jobs',
    '_read_run',
    '_string',
    '_validate_request',
    '_verify_tree',
    '_workflow_path',
    'build_opener',
    'dataclass',
    'fetch_github_ci_evidence',
    'json',
    'logging',
    're',
]
