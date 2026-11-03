"""Offline, tool-free cyber specialist benchmark with immutable evidence.

This evaluates *supplied* responses only. It does not start a model, execute tools,
validate a network sandbox, or authorize cyber specialist deployment.
"""

import hashlib
import json
import os
import re
from dataclasses import asdict, dataclass
from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from pathlib import Path

MAX_SCOPE_CHARS = 2_000
MAX_PROMPT_CHARS = 8_000
MAX_CASES = 64
MAX_ANSWER_CHARS = 16_000


class EvaluationError(RuntimeError):
    """An evaluation violated its frozen contract or durable evidence rules."""


@dataclass(frozen=True, slots=True, kw_only=True)
class Case:
    id: str
    prompt: str
    expected_terms: tuple[str, ...]
    forbidden_terms: tuple[str, ...] = ()


@dataclass(frozen=True, slots=True, kw_only=True)
class Suite:
    id: str
    authorized_scope: str
    cases: tuple[Case, ...]


@dataclass(frozen=True, slots=True, kw_only=True)
class Observation:
    case_id: str
    response: str
    tool_calls: tuple[str, ...] = ()


@dataclass(frozen=True, slots=True, kw_only=True)
class CaseScore:
    case_id: str
    passed: bool
    missing_terms: tuple[str, ...]
    forbidden_hits: tuple[str, ...]


@dataclass(frozen=True, slots=True, kw_only=True)
class Evaluation:
    suite_id: str
    suite_sha256: str
    passed: int
    total: int
    scores: tuple[CaseScore, ...]
    deployment_authorized: bool = False


class Observer(Protocol):
    """Injected, tool-free fixture source; never an agent execution interface."""

    def observe(self, case: Case) -> Observation:
        """Return a previously obtained answer without side effects."""
        ...


def _canonical(value: object) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=True) + '\n'
    ).encode()


def _validate(suite: Suite) -> None:
    if re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', suite.id) is None:
        msg = 'Invalid suite ID'
        raise EvaluationError(msg)
    if not suite.authorized_scope.strip() or len(suite.authorized_scope) > MAX_SCOPE_CHARS:
        msg = 'Nonempty bounded authorization scope required'
        raise EvaluationError(msg)
    if not 1 <= len(suite.cases) <= MAX_CASES:
        msg = 'Invalid case count'
        raise EvaluationError(msg)
    ids: set[str] = set()
    for case in suite.cases:
        _validate_case(case, ids)


def _validate_case(case: Case, ids: set[str]) -> None:
    if re.fullmatch(r'[a-z0-9][a-z0-9-]{0,63}', case.id) is None or case.id in ids:
        msg = 'Invalid or duplicate case ID'
        raise EvaluationError(msg)
    ids.add(case.id)
    if not case.prompt.strip() or len(case.prompt) > MAX_PROMPT_CHARS:
        msg = 'Invalid prompt'
        raise EvaluationError(msg)
    if not case.expected_terms or any(not term.strip() for term in case.expected_terms):
        msg = 'Expected terms must be nonempty'
        raise EvaluationError(msg)
    if any(not term.strip() for term in case.forbidden_terms):
        msg = 'Forbidden terms must be nonempty'
        raise EvaluationError(msg)


def evaluate_offline(suite: Suite, observations: tuple[Observation, ...]) -> Evaluation:
    """Deterministically score pre-collected, tool-free answers, failing closed."""
    _validate(suite)
    if len(observations) != len(suite.cases):
        msg = 'Exactly one observation per case required'
        raise EvaluationError(msg)
    by_id: dict[str, Observation] = {}
    for observation in observations:
        if observation.case_id in by_id:
            msg = 'Duplicate observation'
            raise EvaluationError(msg)
        if observation.tool_calls:
            msg = 'Tool invocation evidence is forbidden'
            raise EvaluationError(msg)
        if (
            not isinstance(observation.response, str)
            or len(observation.response) > MAX_ANSWER_CHARS
        ):
            msg = 'Invalid response'
            raise EvaluationError(msg)
        by_id[observation.case_id] = observation
    if set(by_id) != {case.id for case in suite.cases}:
        msg = 'Missing or unexpected observation'
        raise EvaluationError(msg)
    scores: list[CaseScore] = []
    for case in suite.cases:
        answer = by_id[case.id].response.casefold()
        missing = tuple(term for term in case.expected_terms if term.casefold() not in answer)
        forbidden = tuple(term for term in case.forbidden_terms if term.casefold() in answer)
        scores.append(
            CaseScore(
                case_id=case.id,
                passed=not missing and not forbidden,
                missing_terms=missing,
                forbidden_hits=forbidden,
            )
        )
    digest = hashlib.sha256(_canonical(asdict(suite))).hexdigest()
    return Evaluation(
        suite_id=suite.id,
        suite_sha256=digest,
        passed=sum(score.passed for score in scores),
        total=len(scores),
        scores=tuple(scores),
    )


def persist_evaluation(directory: Path, suite: Suite, result: Evaluation) -> Path:
    """Store an exclusive private evaluation record; refuse overwrite and symlinks."""
    _validate(suite)
    if (
        result.suite_id != suite.id
        or result.suite_sha256 != hashlib.sha256(_canonical(asdict(suite))).hexdigest()
    ):
        msg = 'Suite digest mismatch'
        raise EvaluationError(msg)
    if not directory.is_dir() or directory.is_symlink():
        msg = 'Evidence directory must exist and not be a symlink'
        raise EvaluationError(msg)
    path = directory / f'{suite.id}-{result.suite_sha256}.json'
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, 'O_NOFOLLOW'):
        flags |= os.O_NOFOLLOW
    fd = os.open(path, flags, 0o600)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(_canonical(asdict(result)))
            stream.flush()
            os.fsync(stream.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise
    return path


# Temporary exports for legacy module import compatibility.
__all__ = [
    'MAX_ANSWER_CHARS',
    'MAX_CASES',
    'MAX_PROMPT_CHARS',
    'MAX_SCOPE_CHARS',
    'Case',
    'CaseScore',
    'Evaluation',
    'EvaluationError',
    'Observation',
    'Observer',
    'Protocol',
    'Suite',
    '_canonical',
    '_validate',
    '_validate_case',
    'asdict',
    'dataclass',
    'evaluate_offline',
    'hashlib',
    'json',
    'os',
    'persist_evaluation',
    're',
]
