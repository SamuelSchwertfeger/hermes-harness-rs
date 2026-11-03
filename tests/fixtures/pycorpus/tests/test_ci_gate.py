"""CI assessment tests use synthetic API returns; no GitHub requests."""

import pytest

from hermes_harness.ci.ci_gate import CIGateStatus, assess_github_ci
from hermes_harness.ci.ci_observation import (
    CIPolicy,
)
from hermes_harness.ci.ci_request import CIRequest
from hermes_harness.ci.github_ci_reader import GitHubReadClient
from hermes_harness.core.delegation import DelegationError

_COMMIT = 'a' * 40
_TREE = 'b' * 40
_RUN_ID = 42
_ATTEMPT = 2


@pytest.fixture
def candidate() -> CIRequest:
    return CIRequest(
        'task1',
        'hash',
        'c' * 40,
        _COMMIT,
        _TREE,
        'harness/task1',
        ('x.py',),
        'digest',
    )


@pytest.fixture
def policy() -> CIPolicy:
    return CIPolicy('owner/repo', '.github/workflows/ci.yml', ('unit',))


def _routes(candidate: CIRequest) -> dict[str, dict[str, object]]:
    base = '/repos/owner/repo'
    return {
        f'{base}/actions/runs/{_RUN_ID}': {
            'repository': {'full_name': 'owner/repo'},
            'path': 'owner/repo/.github/workflows/ci.yml@refs/heads/main',
            'head_sha': candidate.commit,
            'id': _RUN_ID,
            'run_attempt': _ATTEMPT,
            'event': 'push',
            'status': 'completed',
            'conclusion': 'success',
        },
        f'{base}/git/commits/{candidate.commit}': {
            'sha': candidate.commit,
            'tree': {'sha': candidate.tree},
        },
        f'{base}/actions/runs/{_RUN_ID}/attempts/{_ATTEMPT}/jobs?per_page=100&page=1': {
            'total_count': 1,
            'jobs': [
                {
                    'id': 100,
                    'run_id': _RUN_ID,
                    'run_attempt': _ATTEMPT,
                    'name': 'unit',
                    'status': 'completed',
                    'conclusion': 'success',
                }
            ],
        },
    }


def test_fresh_read_returns_non_authorizing_pass(
    monkeypatch: pytest.MonkeyPatch,
    candidate: CIRequest,
    policy: CIPolicy,
) -> None:
    routes = _routes(candidate)
    monkeypatch.setattr(GitHubReadClient, '__call__', lambda _self, path: routes[path])
    result = assess_github_ci(candidate, policy, _RUN_ID, github_token='synthetic-test-token')  # noqa: S106 - fake offline credential
    assert result.status is CIGateStatus.OBSERVED_PASS
    assert result.run_attempt == _ATTEMPT
    assert result.tree == candidate.tree


def test_incomplete_is_not_pass(
    monkeypatch: pytest.MonkeyPatch,
    candidate: CIRequest,
    policy: CIPolicy,
) -> None:
    routes = _routes(candidate)
    routes[f'/repos/owner/repo/actions/runs/{_RUN_ID}']['status'] = 'in_progress'
    routes[f'/repos/owner/repo/actions/runs/{_RUN_ID}']['conclusion'] = None
    monkeypatch.setattr(GitHubReadClient, '__call__', lambda _self, path: routes[path])
    result = assess_github_ci(candidate, policy, _RUN_ID, github_token='synthetic-test-token')  # noqa: S106 - fake offline credential
    assert result.status is CIGateStatus.INCOMPLETE


def test_mismatched_tree_is_unknown(
    monkeypatch: pytest.MonkeyPatch,
    candidate: CIRequest,
    policy: CIPolicy,
) -> None:
    routes = _routes(candidate)
    routes[f'/repos/owner/repo/git/commits/{candidate.commit}']['tree'] = {'sha': '0' * 40}
    monkeypatch.setattr(GitHubReadClient, '__call__', lambda _self, path: routes[path])
    result = assess_github_ci(candidate, policy, _RUN_ID, github_token='synthetic-test-token')  # noqa: S106 - fake offline credential
    assert result.status is CIGateStatus.UNKNOWN


def test_transport_failure_is_unknown(
    monkeypatch: pytest.MonkeyPatch,
    candidate: CIRequest,
    policy: CIPolicy,
) -> None:
    def fail(_self: GitHubReadClient, _path: str) -> dict[str, object]:
        msg = 'synthetic API failure'
        raise DelegationError(msg)

    monkeypatch.setattr(GitHubReadClient, '__call__', fail)
    result = assess_github_ci(candidate, policy, _RUN_ID, github_token='synthetic-test-token')  # noqa: S106 - fake offline credential
    assert result.status is CIGateStatus.UNKNOWN
    assert result.run_attempt is None


def test_wrong_run_id_rejected_without_fetch(candidate: CIRequest, policy: CIPolicy) -> None:
    with pytest.raises(DelegationError, match='positive GitHub Actions run ID'):
        assess_github_ci(candidate, policy, 0, github_token='synthetic-test-token')  # noqa: S106 - fake offline credential


def test_no_token_rejected(candidate: CIRequest, policy: CIPolicy) -> None:
    with pytest.raises(DelegationError, match='token is required'):
        assess_github_ci(candidate, policy, _RUN_ID, github_token='')


def test_unsuccessful_job_is_rejected(
    monkeypatch: pytest.MonkeyPatch,
    candidate: CIRequest,
    policy: CIPolicy,
) -> None:
    routes = _routes(candidate)
    job_path = (
        f'/repos/owner/repo/actions/runs/{_RUN_ID}/attempts/{_ATTEMPT}/jobs?per_page=100&page=1'
    )
    jobs = routes[job_path]['jobs']
    assert isinstance(jobs, list)
    assert isinstance(jobs[0], dict)
    jobs[0]['conclusion'] = 'failure'
    monkeypatch.setattr(GitHubReadClient, '__call__', lambda _self, path: routes[path])
    result = assess_github_ci(candidate, policy, _RUN_ID, github_token='synthetic-test-token')  # noqa: S106 - fake offline credential
    assert result.status is CIGateStatus.REJECTED
