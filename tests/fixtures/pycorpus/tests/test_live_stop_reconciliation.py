"""Stop reconciliation never calls systemd-stop twice or relaunches a service."""

from typing import TYPE_CHECKING

import pytest

from hermes_harness.services.live_service_controller import ServiceControlError, ServiceController
from hermes_harness.services.live_service_journal import ServiceJournal, ServiceState

if TYPE_CHECKING:
    from pathlib import Path


class ObservationTransport:
    """Only status results are configurable; invoke must never be used."""

    def __init__(self) -> None:
        self.inactive = False
        self.calls: list[str] = []
        self.fail = False

    def invoke(self, _argv: tuple[str, ...]) -> None:
        msg = 'Unexpected service side effect during recovery'
        raise AssertionError(msg)

    def is_active(self, _unit: str) -> bool:
        msg = 'Recovery must not request an active observation'
        raise AssertionError(msg)

    def is_inactive(self, unit: str) -> bool:
        self.calls.append(unit)
        if self.fail:
            msg = 'simulated systemd inspection failure'
            raise OSError(msg)
        return self.inactive


def _controller(
    tmp_path: Path, *, state: ServiceState
) -> tuple[ServiceController, ObservationTransport]:
    journal = ServiceJournal(tmp_path / 'services.sqlite')
    journal.reserve(unit='hermes-job-stop', role='worker', plan_json='{}')
    if state != ServiceState.LAUNCH_UNCERTAIN:
        journal.transition(
            'hermes-job-stop',
            expected=ServiceState.LAUNCH_UNCERTAIN,
            target=ServiceState.RUNNING_OBSERVED,
        )
    if state == ServiceState.STOP_UNCERTAIN:
        journal.transition(
            'hermes-job-stop',
            expected=ServiceState.RUNNING_OBSERVED,
            target=ServiceState.STOP_UNCERTAIN,
        )
    transport = ObservationTransport()
    return ServiceController(journal, transport), transport


def test_reconciles_uncertain_stop_only_after_inactive_observation(tmp_path: Path) -> None:
    controller, transport = _controller(tmp_path, state=ServiceState.STOP_UNCERTAIN)
    transport.inactive = True
    result = controller.reconcile_stopped('hermes-job-stop')
    assert result.state == ServiceState.STOPPED_OBSERVED
    assert transport.calls == ['hermes-job-stop']
    assert ServiceJournal(controller.journal.path).get('hermes-job-stop') == result


def test_unknown_stop_remains_durable(tmp_path: Path) -> None:
    controller, transport = _controller(tmp_path, state=ServiceState.STOP_UNCERTAIN)
    with pytest.raises(ServiceControlError, match='still unknown'):
        controller.reconcile_stopped('hermes-job-stop')
    assert controller.inspect('hermes-job-stop').state == ServiceState.STOP_UNCERTAIN  # type: ignore[union-attr]
    assert transport.calls == ['hermes-job-stop']


def test_transport_failure_does_not_clear_uncertainty(tmp_path: Path) -> None:
    controller, transport = _controller(tmp_path, state=ServiceState.STOP_UNCERTAIN)
    transport.fail = True
    with pytest.raises(OSError, match='simulated systemd inspection failure'):
        controller.reconcile_stopped('hermes-job-stop')
    assert controller.inspect('hermes-job-stop').state == ServiceState.STOP_UNCERTAIN  # type: ignore[union-attr]


def test_launch_uncertain_cannot_become_running_via_stop_recovery(tmp_path: Path) -> None:
    controller, transport = _controller(tmp_path, state=ServiceState.LAUNCH_UNCERTAIN)
    transport.inactive = True
    with pytest.raises(ServiceControlError, match='previously uncertain stop'):
        controller.reconcile_stopped('hermes-job-stop')
    assert transport.calls == []


def test_running_service_cannot_be_stopped_implicitly(tmp_path: Path) -> None:
    controller, transport = _controller(tmp_path, state=ServiceState.RUNNING_OBSERVED)
    transport.inactive = True
    with pytest.raises(ServiceControlError, match='previously uncertain stop'):
        controller.reconcile_stopped('hermes-job-stop')
    assert transport.calls == []


def test_second_reconciliation_denied_without_probing_again(tmp_path: Path) -> None:
    controller, transport = _controller(tmp_path, state=ServiceState.STOP_UNCERTAIN)
    transport.inactive = True
    controller.reconcile_stopped('hermes-job-stop')
    with pytest.raises(ServiceControlError, match='previously uncertain stop'):
        controller.reconcile_stopped('hermes-job-stop')
    assert transport.calls == ['hermes-job-stop']
