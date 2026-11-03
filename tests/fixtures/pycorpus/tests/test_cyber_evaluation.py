"""Cyber evaluation is deliberately offline and has no dispatch hook."""

import json
from dataclasses import replace
from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    from pathlib import Path

from hermes_harness.evaluation.cyber_evaluation import (
    Case,
    EvaluationError,
    Observation,
    Suite,
    evaluate_offline,
    persist_evaluation,
)

PRIVATE_FILE_MODE = 0o600


def _suite() -> Suite:
    return Suite(
        id='scope-1',
        authorized_scope='Synthetic fixtures only; no target access',
        cases=(
            Case(
                id='case-1',
                prompt='Identify fixture finding',
                expected_terms=('risk',),
                forbidden_terms=('exploit',),
            ),
        ),
    )


def test_fixture_pass_is_not_deployment_authorization(tmp_path: Path) -> None:
    suite = _suite()
    result = evaluate_offline(
        suite, (Observation(case_id='case-1', response='RISK is documented'),)
    )
    assert result.passed == result.total == 1
    assert not result.deployment_authorized
    path = persist_evaluation(tmp_path, suite, result)
    assert path.stat().st_mode & 0o777 == PRIVATE_FILE_MODE
    assert json.loads(path.read_text())['deployment_authorized'] is False
    with pytest.raises(FileExistsError):
        persist_evaluation(tmp_path, suite, result)


def test_missing_and_forbidden_are_recorded() -> None:
    result = evaluate_offline(_suite(), (Observation(case_id='case-1', response='exploit'),))
    assert result.passed == 0
    assert result.scores[0].missing_terms == ('risk',)
    assert result.scores[0].forbidden_hits == ('exploit',)


@pytest.mark.parametrize(
    'observations',
    [
        (),
        (Observation(case_id='case-1', response='risk'),) * 2,
        (Observation(case_id='unknown', response='risk'),),
    ],
)
def test_missing_duplicate_or_unknown_rejected(observations: tuple[Observation, ...]) -> None:
    with pytest.raises(EvaluationError):
        evaluate_offline(_suite(), observations)


def test_tool_calls_rejected() -> None:
    with pytest.raises(EvaluationError, match='Tool'):
        evaluate_offline(
            _suite(), (Observation(case_id='case-1', response='risk', tool_calls=('shell',)),)
        )


def test_suite_mutation_rejected_when_persisting(tmp_path: Path) -> None:
    suite = _suite()
    result = evaluate_offline(suite, (Observation(case_id='case-1', response='risk'),))
    changed = replace(suite, authorized_scope='Other target')
    with pytest.raises(EvaluationError, match='digest'):
        persist_evaluation(tmp_path, changed, result)


def test_path_and_scope_validation(tmp_path: Path) -> None:
    suite = replace(_suite(), id='../outside')
    with pytest.raises(EvaluationError):
        evaluate_offline(suite, (Observation(case_id='case-1', response='risk'),))
    suite = replace(_suite(), authorized_scope=' ')
    with pytest.raises(EvaluationError):
        evaluate_offline(suite, (Observation(case_id='case-1', response='risk'),))
    suite = _suite()
    result = evaluate_offline(suite, (Observation(case_id='case-1', response='risk'),))
    link = tmp_path / 'link'
    link.symlink_to(tmp_path, target_is_directory=True)
    with pytest.raises(EvaluationError, match='symlink'):
        persist_evaluation(link, suite, result)
