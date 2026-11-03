"""Offline integration tests for attachment plus read-only admission recovery."""

import asyncio
from contextlib import asynccontextmanager
from dataclasses import dataclass
from typing import TYPE_CHECKING

from hermes_harness.core.reconciliation import ReconciliationStatus
from hermes_harness.gateway.admission_store import AdmissionStore, LocalState
from hermes_harness.gateway.gateway_attachment import GatewayAttachmentConfig
from hermes_harness.gateway.gateway_identity import Receipt, Submission
from hermes_harness.gateway.gateway_live_recovery import recover_with_existing_gateway

if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from pathlib import Path


@dataclass(frozen=True, slots=True)
class Discovery:
    state: str
    endpoint: object | None
    reason_code: str | None = None


class FakeRPC:
    def __init__(self, *, pending: list[object], receipt_id: str = 'adm-1') -> None:
        self.pending = pending
        self.receipt_id = receipt_id
        self.calls: list[tuple[str, dict[str, object]]] = []

    async def rpc(self, method: str, **params: object) -> object:
        self.calls.append((method, params))
        if method == 'session.events.since':
            if 'replay_epoch' not in params:
                return {
                    'events': [],
                    'latest_seq': 1,
                    'last_sequence': 1,
                    'epoch': 'epoch-1',
                    'replay_epoch': 'epoch-1',
                    'truncated': True,
                    'snapshot_required': True,
                    'count': 0,
                }
            return {
                'events': [
                    {
                        'type': 'session.info',
                        'session_id': 'session-1',
                        'payload': {'pending': self.pending},
                        'seq': 1,
                        'replay_epoch': 'epoch-1',
                    }
                ],
                'latest_seq': 1,
                'last_sequence': 1,
                'epoch': 'epoch-1',
                'replay_epoch': 'epoch-1',
                'truncated': False,
                'snapshot_required': False,
                'count': 1,
            }
        if method == 'prompt.receipt':
            return {'admission_id': self.receipt_id, 'status': 'queued'}
        msg = f'unexpected RPC: {method}'
        raise AssertionError(msg)


class FakeGateway:
    def __init__(self, rpc: FakeRPC, *, attachable: bool = True) -> None:
        self.rpc = rpc
        self.attachable_result = attachable
        self.discovery_calls: list[str] = []
        self.connector_endpoints: list[object] = []

    def discover(self, profile_home: str) -> Discovery:
        self.discovery_calls.append(profile_home)
        return Discovery('ready', 'endpoint-1')

    def is_attachable(self, _result: Discovery) -> bool:
        return self.attachable_result

    @asynccontextmanager
    async def connector(self, *, endpoint: object) -> AsyncIterator[FakeRPC]:
        self.connector_endpoints.append(endpoint)
        yield self.rpc

    def config(self, profile_home: str) -> GatewayAttachmentConfig:
        return GatewayAttachmentConfig(
            profile_home=profile_home,
            discover=self.discover,
            is_attachable=self.is_attachable,
            connector=self.connector,
        )


def unknown_store(path: Path, *, known: bool = False) -> AdmissionStore:
    store = AdmissionStore(path / 'admissions.sqlite')
    store.prepare(Submission('session-1', 'sub-1'))
    if known:
        store.record_receipt('sub-1', Receipt('adm-1', 'queued'))
    store.mark_outcome_unknown('sub-1')
    return store


def test_live_recovery_attaches_once_and_verifies_candidate(tmp_path: Path) -> None:
    store = unknown_store(tmp_path)
    rpc = FakeRPC(pending=[{'input_id': 'sub-1', 'admission_id': 'adm-1'}])
    gateway = FakeGateway(rpc)

    status = asyncio.run(
        recover_with_existing_gateway(store, 'sub-1', config=gateway.config(str(tmp_path)))
    )

    assert status is ReconciliationStatus.RECEIPT_RECORDED
    record = store.get('sub-1')
    assert record is not None
    assert record.admission_id == 'adm-1'
    assert record.local_state is not LocalState.OUTCOME_UNKNOWN
    assert gateway.discovery_calls == [str(tmp_path)]
    assert gateway.connector_endpoints == ['endpoint-1']
    assert [method for method, _ in rpc.calls] == [
        'session.events.since',
        'session.events.since',
        'prompt.receipt',
    ]


def test_ineligible_record_does_not_discover_or_connect(tmp_path: Path) -> None:
    store = AdmissionStore(tmp_path / 'admissions.sqlite')
    rpc = FakeRPC(pending=[])
    gateway = FakeGateway(rpc)

    status = asyncio.run(
        recover_with_existing_gateway(store, 'missing', config=gateway.config(str(tmp_path)))
    )

    assert status is ReconciliationStatus.NOT_ELIGIBLE
    assert gateway.discovery_calls == []
    assert gateway.connector_endpoints == []
    assert rpc.calls == []
