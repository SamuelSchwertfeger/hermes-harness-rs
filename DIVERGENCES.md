# Rust divergences from the Python original

- **NUL-delimited Git paths retain whitespace.** Rust leaves output from Git's
  `-z` commands untouched and drops only the trailing empty split entry. Trimming
  can disguise an unowned path such as ` src/x` as `src/x`.
  Python reference: `hermes_harness/git/worktrees.py:54` strips all Git output.

- **Empty Git `-z` entries.** Git path lists split on NUL now drop empty
  entries like Python did. An early port kept them (found by the differential
  suite, finding 6).

- **Paths converted to command arguments or recorded identities require UTF-8.**
  Sandbox plans, worktree mutation arguments, and task reservation identities
  reject non-UTF-8 paths with `InvalidInput` instead of substituting replacement
  characters, so distinct filesystem paths cannot collapse to one identity.
  Python references: `hermes_harness/sandbox/live_sandbox_plan.py:125`,
  `hermes_harness/git/worktrees.py:99`, and
  `hermes_harness/core/task_reservation.py:127-128` use `str(path)`;
  Python can retain filesystem surrogate escapes that Rust UTF-8 strings cannot.

- **Operator probes require an explicit endpoint and fail closed in production.**
  `hermes-probe` replaces seven removed scripts. Gateway operations require
  `--endpoint <unix socket path>`; offline admission/reconciliation preflight and
  help need none. No discovery, autostart, ensure, reconnect or retry
  is implemented. `gateway_attachment.rs` only defines injected clients;
  `gateway_socket_qualification.rs` uses a private token echo, not Hermes RPC
  framing or authentication. The production connector on every platform
  exits 2 with `Hermes gateway client unavailable in Rust build; use the Python probe`.
  Tests inject `GatewayRpc` through an attach-only connector and adapt it to
  `SessionRpc` without changing the public interfaces. Recovery uses
  `recover_with_async_rpc`, the same read-only implementation called by
  `recover_with_existing_gateway`, after the explicit connection.

- **Probe output and parsing.** Gateway probe successes and failures retain JSON
  metadata and exit statuses 0/2. Local authorization refusals retain Python's
  messages in `ProbeError`; the CLI emits only the bounded error category, as in
  Python. Transport errors wrapped by existing modules use `RuntimeError` rather
  than preserving every Python exception subclass. Parser errors go to stderr
  with exit 2; wording differs from argparse. Long option abbreviations and
  `--option=value` are accepted. Paths passed as separate values retain OS-native
  bytes; non-UTF-8 inline options are rejected.

- **`ollama-smoke` output.** The smoke probe emits the bounded proposal/review as
  JSON instead of headings. It preserves the worker/reviewer models, budgets,
  schemas, objective, and no-application behavior. Python AST syntax validation
  is unavailable without a Python parser and is reported as
  `python_syntax_checked: false` rather than claiming syntax success. Smoke and
  fixture runtime failures use the common sanitized JSON boundary and exit 2
  instead of an uncaught Python exception and exit 1.

- **Fixture locations and platform behavior.** Recovery creates its random,
  exclusive 0700 directory in the system temporary directory, as Python does;
  its SQLite files and directory are removed on both success and failure. The
  trusted functional fixture creates its directory the same way and runs the
  same pass/fail Python unittest fixtures through `functional_verify` on Unix.
  It retains the fixture repositories, checkpoints, and evidence for operator
  inspection rather than recursively deleting them, reporting
  `evidence_directory` on success.
  Fixture Git setup uses per-process identity variables instead of `git -c/-C`.
  Its output is bounded JSON rather than printed evidence fields. Non-Unix
  builds fail closed with exit 2; no host execution fallback is provided.

- **Probe CLI surface.** `--profile-home` is still required and must be
  nonblank, but it no longer locates the gateway: the operator passes the
  socket with `--endpoint`, and the two are not cross-checked. Output is
  compact JSON (`{"a":1}`), not `json.dumps` spacing (`{"a": 1}`); parse it,
  don't text-match it. Every gateway-probe failure, including library value
  errors such as store conflicts, exits 2 with a sanitized `RuntimeError` or
  `ValueError` category, where Python let unexpected exceptions escape as a
  traceback with exit 1. Option values other than paths must be UTF-8.

- **Stricter JSON decoding of untrusted input.** The broker and Ollama
  decoders accept only UTF-8 without a BOM; Python's `json.loads(bytes)` also
  auto-detected UTF-16/32 and a UTF-8 BOM. Lone surrogate escapes, NaN and
  Infinity literals, nesting of 128 or deeper, integers above `u64::MAX`, and
  a `-0` model size are rejected even in ignored fields. Each case was found
  by the differential suite and is listed in `tests/differential/FINDINGS.md`.

- **Structured logs.** Python used `logging` with default handlers. Rust emits
  nothing unless `HERMES_LOG` is set, and then writes one JSON object per line
  to stderr (`ts`, `level`, `target`, `msg`, `bin`). Binary start and exit
  (code, elapsed ms) are logged at `info`; arguments never are.
- **Preflight identity check.** Upstream compared `HOME` with one operator's personal
  home directory. The published copy (Rust and the vendored Python) uses the
  placeholder `/home/operator`, so on a real host the check reduces to "not root".
  Set the placeholder to your operator's home if you rely on it.
