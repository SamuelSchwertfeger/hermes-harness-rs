"""Read-only GitHub API reader tests with injected responses; no network."""

from dataclasses import replace

import pytest

from hermes_harness.ci.ci_observation import CIEvaluation, CIPolicy, evaluate_ci_observation
from hermes_harness.ci.ci_request import CIRequest
from hermes_harness.ci.github_ci_reader import fetch_github_ci_evidence
from hermes_harness.core.delegation import DelegationError

_COMMIT = 'a' * 40
_TREE = 'b' * 40


@pytest.fixture
def inputs() -> tuple[CIRequest, CIPolicy, dict[str, dict[str, object]]]:
    candidate = CIRequest(
        't', 'digest', 'c' * 40, _COMMIT, _TREE, 'harness/t', ('src/x.py',), 'digest'
    )
    policy = CIPolicy('owner/repo', '.github/workflows/ci.yml', ('quality',))
    root = '/repos/owner/repo'
    run = {
        'id': 45,
        'run_attempt': 2,
        'path': 'owner/repo/.github/workflows/ci.yml@refs/heads/main',
        'repository': {'full_name': 'owner/repo'},
        'head_sha': _COMMIT,
        'event': 'push',
        'status': 'completed',
        'conclusion': 'success',
    }
    jobs = {
        'total_count': 1,
        'jobs': [
            {
                'id': 55,
                'run_id': 45,
                'run_attempt': 2,
                'name': 'quality',
                'status': 'completed',
                'conclusion': 'success',
            }
        ],
    }
    routes = {
        f'{root}/actions/runs/45': run,
        f'{root}/git/commits/{_COMMIT}': {'sha': _COMMIT, 'tree': {'sha': _TREE}},
        f'{root}/actions/runs/45/attempts/2/jobs?per_page=100&page=1': jobs,
    }
    return candidate, policy, routes


def test_complete_observation_remains_untrusted(inputs: tuple) -> None:
    candidate, policy, routes = inputs
    result = fetch_github_ci_evidence(candidate, policy, 45, reader=routes.__getitem__)
    assert result.run.run_attempt == routes['/repos/owner/repo/actions/runs/45']['run_attempt']
    assert evaluate_ci_observation(candidate, result, policy) is CIEvaluation.PASSING_OBSERVATION


@pytest.mark.parametrize(
    ('field', 'value'),
    [('head_sha', '0' * 40), ('run_attempt', 0), ('id', 46)],
)
def test_run_mismatch_fails_closed(inputs: tuple, field: str, value: object) -> None:
    candidate, policy, routes = inputs
    routes['/repos/owner/repo/actions/runs/45'][field] = value
    with pytest.raises(DelegationError):
        fetch_github_ci_evidence(candidate, policy, 45, reader=routes.__getitem__)


def test_tree_mismatch_fails_closed(inputs: tuple) -> None:
    candidate, policy, routes = inputs
    routes[f'/repos/owner/repo/git/commits/{_COMMIT}']['tree'] = {'sha': '0' * 40}
    with pytest.raises(DelegationError, match='tree'):
        fetch_github_ci_evidence(candidate, policy, 45, reader=routes.__getitem__)


def test_incomplete_pagination_fails_closed(inputs: tuple) -> None:
    candidate, policy, routes = inputs
    routes['/repos/owner/repo/actions/runs/45/attempts/2/jobs?per_page=100&page=1'][
        'total_count'
    ] = 2
    routes['/repos/owner/repo/actions/runs/45/attempts/2/jobs?per_page=100&page=2'] = {
        'total_count': 2,
        'jobs': [],
    }
    with pytest.raises(DelegationError, match='pagination'):
        fetch_github_ci_evidence(candidate, policy, 45, reader=routes.__getitem__)


def test_duplicate_job_ids_fails_closed(inputs: tuple) -> None:
    candidate, policy, routes = inputs
    key = '/repos/owner/repo/actions/runs/45/attempts/2/jobs?per_page=100&page=1'
    jobs = routes[key]['jobs']
    routes[key]['jobs'] = [*jobs, *jobs]
    routes[key]['total_count'] = 2
    with pytest.raises(DelegationError, match='Duplicate'):
        fetch_github_ci_evidence(candidate, policy, 45, reader=routes.__getitem__)


def test_invalid_repository_blocks_reader(inputs: tuple) -> None:
    candidate, policy, routes = inputs
    with pytest.raises(DelegationError, match='repository'):
        fetch_github_ci_evidence(
            candidate, replace(policy, repository='../x'), 45, reader=routes.__getitem__
        )


def test_no_token_injection_or_live_fetch(inputs: tuple) -> None:
    candidate, policy, routes = inputs
    paths: list[str] = []

    def reader(path: str) -> dict[str, object]:
        paths.append(path)
        return routes[path]

    fetch_github_ci_evidence(candidate, policy, 45, reader=reader)
    assert len(paths) == len(routes)
    assert all(path.startswith('/repos/owner/repo/') for path in paths)
