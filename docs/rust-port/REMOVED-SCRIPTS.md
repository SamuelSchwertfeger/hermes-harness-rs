# Removed script replacements

The operator scripts are subcommands of the `hermes-probe` binary (internal library
module `src/rs/probe/`). Each subcommand supports `--help`.

| Removed script | Replacement |
| --- | --- |
| `scripts/probe_disposable_admission.py` | `hermes-probe disposable-admission` |
| `scripts/probe_live_admission.py` | `hermes-probe live-admission` |
| `scripts/probe_unknown_recovery.py` | `hermes-probe unknown-recovery` |
| `scripts/reconcile_disposable_terminal.py` | `hermes-probe reconcile-terminal` |
| `scripts/validate_existing_hermes.py` | `hermes-probe validate-existing` |
| `scripts/smoke_ollama_native.py` | `hermes-probe ollama-smoke` |
| `scripts/hermes-functional-live-fixture.py` | `hermes-probe functional-fixture` |
| `scripts/qualify_gateway_socket.py` | `hermes-gateway-socket-qualification` (existing binary) |
| `scripts/apply_package_cleanup.sh` | None: obsolete one-shot package cleanup |

Gateway operations require an explicit `--endpoint <unix socket path>` and never
discover, start, reconnect, or ensure a runtime. Offline preflight needs no endpoint.
The production connector fails closed because the repository does not define the
Hermes client protocol; tests drive the complete workflows through injected connectors.
See `DIVERGENCES.md` for platform, output, and fixture retention differences.
