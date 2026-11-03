"""Read-only host control inspection; no service launch or automatic approval."""

import argparse

from hermes_harness.operator.live_host_probe import (
    ProbeCommands,
    assess_observation,
    observe_unit,
    operator_runner,
)


def main() -> int:
    """Inspect a named disposable unit; no live Hermes or provider execution."""
    parser = argparse.ArgumentParser(description='Inspect fixed systemd sandbox properties')
    parser.add_argument('--unit', required=True)
    parser.add_argument('--execute-readonly', action='store_true')
    args = parser.parse_args()
    commands = ProbeCommands(args.unit)
    if not args.execute_readonly:
        parser.error('Pass --execute-readonly to query systemd; default is disabled')
    values = observe_unit(commands, operator_runner)
    failures = assess_observation(values)
    for name in sorted(values):
        print(f'{name}={values[name]}')
    print('Control mismatches: ' + (', '.join(failures) if failures else 'none'))
    print('Gateway authorization: NOT GRANTED')
    return 1 if failures else 0


if __name__ == '__main__':
    raise SystemExit(main())

# Temporary exports for legacy module import compatibility.
__all__ = [
    'ProbeCommands',
    'argparse',
    'assess_observation',
    'main',
    'observe_unit',
    'operator_runner',
]
