# Sandboxed local-agent tools (experimental)

`src/rs/multiagent/sandbox_tools.rs` is a read-only, allowlisted Bubblewrap
command runner. It is tested offline and has not been qualified on a live host.

Supported operations: `list-files`, `search-fixed` (a bounded literal pattern)
and `git-status`. The caller supplies an approved, existing absolute worktree,
a separate authorization and a new receipt path. The process gets no host home,
no network, no secrets, a read-only worktree mount, a private `/tmp`, fixed
command arguments, CPU and address-space limits and bounded stdout and stderr
capture. Failures never fall back to host execution.

Two entry points use it: `hermes-skill-bridge sandbox-tool --authorize-execution`
runs one operation for an operator, and `hermes-skill-bridge assemble-live
--authorize-tools` lets the live worker request at most three of these
operations per node.

Not implemented: writes, multi-node tool loops and arbitrary model-issued
commands. Do not treat a receipt as evidence that model-issued commands are
safe. Security qualification needs negative runtime tests on the deployment
host, covering symlink traversal, host secrets, network isolation, timeouts and
process limits. `--ro-bind` alone does not constrain a model that can run an
arbitrary shell; only the fixed commands above are accepted.
