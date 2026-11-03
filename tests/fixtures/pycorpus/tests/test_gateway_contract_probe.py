"""Offline tests of the Hermes RPC source inspector (no inference or imports)."""

from typing import TYPE_CHECKING

import pytest

from hermes_harness.gateway.gateway_contract_probe import (
    ContractInspectionError,
    inspect_checkout,
    inspect_source,
)

if TYPE_CHECKING:
    from pathlib import Path

SOURCE = """
class Gateway:
    async def submit(self, params):
        return {'admission_id': 'abc', 'state': 'queued'}
    def receipt(self, params):
        return {'submission_id': 'def', 'state': 'running'}
    def cancel(self, params):
        return {'status': 'requested'}
"""


def test_extracts_method_hints_without_claiming_verification() -> None:
    report = inspect_source(SOURCE)
    assert [method.name for method in report.methods] == ['submit', 'receipt', 'cancel']
    assert report.methods[0].parameters == ('self', 'params')
    assert 'admission_id' in report.methods[0].literal_dict_keys
    assert report.verified is False
    assert report.live_dispatch_enabled is False


def test_detects_missing_or_duplicate_handlers() -> None:
    with pytest.raises(ContractInspectionError, match='ambiguous'):
        inspect_source('def submit(): pass')
    with pytest.raises(ContractInspectionError, match='ambiguous'):
        inspect_source(SOURCE + '\ndef cancel(): pass\n')


def test_rejects_unparseable_input() -> None:
    with pytest.raises(ContractInspectionError, match='Cannot parse'):
        inspect_source('def submit(??')


def test_checkout_reads_only_expected_source(tmp_path: Path) -> None:
    gateway = tmp_path / 'gateway'
    gateway.mkdir()
    (gateway / 'session_controls.py').write_text(SOURCE)
    report = inspect_checkout(tmp_path)
    assert report.source == 'gateway/session_controls.py'
    assert report.methods[0].literal_dict_keys == ('admission_id', 'state')


def test_rejects_symlink_source(tmp_path: Path) -> None:
    gateway = tmp_path / 'gateway'
    gateway.mkdir()
    target = tmp_path / 'outside.py'
    target.write_text(SOURCE)
    (gateway / 'session_controls.py').symlink_to(target)
    with pytest.raises(ContractInspectionError, match='symlinked'):
        inspect_checkout(tmp_path)
