"""Offline tests for bounded Hermes source lookup inspection."""

from typing import TYPE_CHECKING

import pytest

if TYPE_CHECKING:
    from pathlib import Path

from hermes_harness.gateway.gateway_lookup_probe import LookupProbeError, ScanLimits, inspect_tree


def test_finds_submission_id_lookup_candidate(tmp_path: Path) -> None:
    source = tmp_path / 'gateway.py'
    source.write_text(
        """class Admissions:
    def find_by_submission_id(self, submission_id: str):
        return self.db.execute('SELECT * FROM admissions WHERE submission_id = ?', (submission_id,))
"""
    )

    report = inspect_tree(tmp_path)

    assert report.files_scanned == 1
    assert report.capability_verified is False
    assert any(
        finding.symbol == 'Admissions.find_by_submission_id'
        and 'submission_id' in finding.identifiers
        for finding in report.findings
    )


def test_ignores_unrelated_functions(tmp_path: Path) -> None:
    (tmp_path / 'plain.py').write_text('def health():\n    return {"ok": True}\n')
    report = inspect_tree(tmp_path)
    assert report.findings == ()


def test_reports_parse_failure_without_executing_source(tmp_path: Path) -> None:
    (tmp_path / 'broken.py').write_text('def find_by_submission_id(\n')
    report = inspect_tree(tmp_path)
    assert len(report.parse_failures) == 1
    assert report.parse_failures[0].path == 'broken.py'


def test_skips_symlinked_python_files(tmp_path: Path) -> None:
    outside = tmp_path.parent / 'outside_lookup_probe.py'
    outside.write_text('def find_by_submission_id(submission_id):\n    return submission_id\n')
    (tmp_path / 'linked.py').symlink_to(outside)
    report = inspect_tree(tmp_path)
    assert report.files_scanned == 0
    assert report.findings == ()


def test_enforces_file_limit(tmp_path: Path) -> None:
    (tmp_path / 'one.py').write_text('x = 1\n')
    (tmp_path / 'two.py').write_text('y = 2\n')
    with pytest.raises(LookupProbeError, match='file limit'):
        inspect_tree(tmp_path, limits=ScanLimits(max_files=1))


def test_enforces_total_byte_limit(tmp_path: Path) -> None:
    (tmp_path / 'one.py').write_text('x = 12345\n')
    report = inspect_tree(tmp_path, limits=ScanLimits(max_total_bytes=1))
    assert report.files_scanned == 0
    assert report.truncated is True
