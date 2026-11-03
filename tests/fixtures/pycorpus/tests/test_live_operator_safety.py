"""Provider-free containment tests for supervised sandbox plans."""

import pytest

from hermes_harness.operator.live_operator_safety import (
    OperatorSafetyError,
    extract_mounts,
    validate_role_sandbox,
    validate_separate_roles,
)

BASE = (
    '/usr/bin/systemd-run',
    '--unit=hermes-job-test',
    '/usr/bin/bwrap',
    '--unshare-net',
    '--unshare-pid',
    '--unshare-user',
    '--die-with-parent',
    '--ro-bind',
    '/usr',
    '/usr',
)
WORKER = (
    *BASE,
    '--bind',
    '/safe/worker',
    '/workspace',
    '--ro-bind',
    '/safe/evidence',
    '/evidence',
    '--chdir',
    '/workspace',
    '/usr/bin/python3',
)
REVIEWER = (
    *BASE,
    '--ro-bind',
    '/safe/review',
    '/workspace',
    '--ro-bind',
    '/safe/evidence',
    '/evidence',
    '--chdir',
    '/workspace',
    '/usr/bin/python3',
)


def test_separate_sandbox_mounts_accepted() -> None:
    validate_separate_roles(WORKER, REVIEWER)
    assert extract_mounts(WORKER).writable == (('/safe/worker', '/workspace'),)


def test_reviewer_writable_mount_denied() -> None:
    with pytest.raises(OperatorSafetyError, match='read-only'):
        validate_role_sandbox(
            (*REVIEWER[:-3], '--bind', '/safe/extra', '/extra', *REVIEWER[-3:]), role='reviewer'
        )


def test_flag_in_command_arguments_does_not_qualify_mount() -> None:
    broken = (
        *BASE,
        '--ro-bind',
        '/safe/evidence',
        '/evidence',
        '--chdir',
        '/workspace',
        '/usr/bin/python3',
        '--bind',
    )
    with pytest.raises(OperatorSafetyError, match='writable'):
        validate_role_sandbox(broken, role='worker')


def test_reviewer_worker_workspace_alias_denied() -> None:
    unsafe = tuple('/safe/worker' if value == '/safe/review' else value for value in REVIEWER)
    with pytest.raises(OperatorSafetyError, match='worker writable workspace'):
        validate_separate_roles(WORKER, unsafe)


def test_missing_evidence_denied() -> None:
    broken = (*BASE, '--bind', '/safe/worker', '/workspace', '--chdir', '/workspace')
    with pytest.raises(OperatorSafetyError, match='Evidence'):
        validate_role_sandbox(broken, role='worker')


def test_duplicate_mount_source_denied() -> None:
    broken = tuple('/safe/worker' if value == '/safe/evidence' else value for value in WORKER)
    with pytest.raises(OperatorSafetyError, match='reused'):
        validate_role_sandbox(broken, role='worker')
