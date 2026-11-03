# Python test-gap disposition

The five tests in `test_gateway_receipt_identity_matrix.py` live in
`tests/gateway_async_recovery.rs` under their original names without `test_`. The 12
removed-script tests live in `tests/probes.rs`; three other name mismatches are listed below.
Python paths are relative to `tests/fixtures/pycorpus/tests/`, Rust paths to the repository root.
Deleted script inventory: [REMOVED-SCRIPTS.md](REMOVED-SCRIPTS.md).

| Python file | Python test | Disposition / covering Rust tests |
| --- | --- | --- |
| `test_disposable_admission_cli.py` | `test_preflight_is_offline_and_creates_no_records` | `tests/probes.rs::preflight_is_offline_and_creates_no_records` |
| `test_disposable_admission_cli.py` | `test_bad_confirmation_rejected_before_gateway` | `tests/probes.rs::bad_confirmation_rejected_before_gateway` |
| `test_disposable_admission_cli.py` | `test_missing_disposable_ack_rejected` | `tests/probes.rs::missing_disposable_ack_rejected` |
| `test_disposable_admission_cli.py` | `test_reused_durable_paths_rejected` | `tests/probes.rs::reused_durable_paths_rejected` |
| `test_disposable_admission_cli.py` | `test_no_gateway_autostart_or_hidden_submission` | `tests/probes.rs::no_gateway_autostart_or_hidden_submission` |
| `test_live_admission_cli.py` | `test_live_probe_requires_explicit_submit_flag` | `tests/probes.rs::live_probe_requires_explicit_submit_flag` |
| `test_live_admission_cli.py` | `test_live_probe_attaches_to_discovered_endpoint_not_ensure_path` | `tests/probes.rs::live_probe_attaches_to_discovered_endpoint_not_ensure_path` |
| `test_live_admission_cli.py` | `test_live_probe_requires_exact_session_confirmation_and_disposable_ack` | `tests/probes.rs::live_probe_requires_exact_session_confirmation_and_disposable_ack` |
| `test_host_recovery_probe.py` | `test_host_probe_has_no_submission_or_cancel_rpc` | `tests/probes.rs::host_probe_has_no_submission_or_cancel_rpc` |
| `test_host_recovery_probe.py` | `test_host_probe_requires_explicit_session_and_temporary_ledger` | `tests/probes.rs::host_probe_requires_explicit_session_and_temporary_ledger` |
| `test_host_recovery_probe.py` | `test_host_probe_is_parseable` | `tests/probes.rs::host_probe_is_parseable` |
| `test_host_contract_cli.py` | `test_host_script_syntax_and_explicit_endpoint` | `tests/probes.rs::host_script_syntax_and_explicit_endpoint` |
| `test_gateway_readonly.py` | `test_async_case` | False positive: parameterized wrapper covered by `tests/gateway_readonly.rs`: `read_only_method_and_identity`, `invalid_session_refused_without_rpc`, `invalid_payload_rejected`, `disconnect_never_retries`, `no_endpoint_does_not_invoke_connector`, `injected_connector_uses_explicit_endpoint_only`. Rust uses synchronous RPC; JSON cannot represent non-string object keys, so the invalid-payload case uses a non-object. |
| `test_task_reservation.py` | `test_symlink_and_mismatched_brief_rejected` | False positive: split into `tests/task_reservation.rs`: `mismatched_brief_rejected`, `symlinked_reservation_rejected` (Unix only). |
| `test_worktrees.py` | `test_symlink_and_existing_workspace_rejected` | False positive: split into `tests/worktrees.rs`: `existing_workspace_rejected`, `symlink_escape_rejected` (Unix only). |

The restored tests use injected connectors and record every RPC. Syntax checks run each
subcommand's `--help` and assert its safety documentation. The endpoint test keeps its original
name but checks an explicit `--endpoint` instead of Python discovery; see DIVERGENCES.md.

## Differential testing

`tests/differential.rs` runs the Rust parsers and the vendored Python originals on the same
generated inputs (mutated valid documents plus hand-written edge cases) and compares accept/reject
and the normalized result. Error messages are not compared. One Python process per pair reads a
JSON-lines batch (`tests/differential/oracle.py`, bytes base64-encoded).

Pairs: broker `decode_request_for_fuzzing` / `_decode_request`; `parse_plan_json`;
`parse_replacement_proposal`; `parse_edit_proposal`; `parse_ollama_tags`;
`splitlines_keepends_for_testing` / `str.splitlines(keepends=True)`;
`is_slug_id_for_testing` / `_ID.fullmatch`; `split_z_paths` / `worktrees.py` `-z` split.

Run (needs Python 3.14; skips and passes when `HH_DIFF_PYTHON` is unset):

```
HH_DIFF_PYTHON=python3 cargo test --locked --test differential -- --nocapture
```

Optional: `HH_DIFF_CASES` (random cases per pair, default 2000), `HH_DIFF_SEED` (reproduce a run).
Known differences are allowlisted in `allow()` and explained in `tests/differential/FINDINGS.md`
(open) or `DIVERGENCES.md` (deliberate). CI job: `diff` in `.github/workflows/rust.yml`.
