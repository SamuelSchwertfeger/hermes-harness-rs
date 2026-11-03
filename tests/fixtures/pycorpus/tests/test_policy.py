from pathlib import Path  # noqa: TC003 - runtime annotation on Python 3.13

import pytest

from hermes_harness.core.ownership import check_paths, check_symlink_boundaries
from hermes_harness.core.routing import Role, select


def test_cyber_disabled() -> None:
    with pytest.raises(PermissionError):
        select(Role.CYBER, authorized_target=True)


def test_ownership(tmp_path: Path) -> None:
    check_paths(['src/pkg/main.py'], ['src/pkg'])
    with pytest.raises(PermissionError):
        check_paths(['secrets/a.env'], ['src/pkg'])
    with pytest.raises(PermissionError):
        check_paths(['../outside'], ['src'])
    (tmp_path / 'link').symlink_to('/etc')
    with pytest.raises(PermissionError):
        check_symlink_boundaries(tmp_path, ['link/passwd'])
