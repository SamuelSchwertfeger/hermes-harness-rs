# Project status

What is implemented and tested offline, what is not qualified on a live host, and the trust boundaries that hold either way. Build, run and test: [README.md](README.md). Differences from the Python: [DIVERGENCES.md](DIVERGENCES.md).

## Implemented and tested offline

Twelve binaries (`[[bin]]` in `Cargo.toml`):

- `hermes-harness`: ticket state, routing and the `multiagent` subcommands.
- `hermes-native`, `hermes-skill-bridge`, `hermes-skill-install`: the commands the Hermes skills call, and skill staging.
- `hermes-probe`: the operator probes from `src/rs/probe/`.
- `hermes-supervised-demo`, `hermes-supervised-status`: offline supervised demo and a read-only view of its evidence.
- `hermes-gateway-contract-probe`, `hermes-gateway-lookup-probe`, `hermes-gateway-socket-qualification`: read-only gateway source inspection and the Unix-socket probe.
- `hermes-live-host-probe`, `hermes-live-operator`: read-only host and cross-ledger inspection.

By package:

- `src/rs/core/`: ticket state and transitions, role routing, delegation, task reservation, ownership checks, foreground execution with a watchdog, a localhost Ollama client.
- `src/rs/native/`, `hermes-native` side: `lets-investigate`, `game-plan`, `agents-assemble --confirm-offline` (deterministic offline workers) and `inspect`.
- `src/rs/native/`, `hermes-skill-bridge` side: authorized `live-review`, `apply-reviewed` into a disposable worktree, `verify-applied`, `checkpoint`, Bubblewrap `functional-verify`, the `repair-*` reservation and dispatch flow, `assemble-live` and `apply-live-assembly`, `sandbox-tool`, and the staged `workflow-*` approvals.
- `hermes_skills/`: six skill manifests (`lets-investigate`, `game-plan`, `agents-assemble`, `what-we-know`, `stick-the-landing`, `cyber-warrior`). `hermes-skill-install` stages the first five; `stick-the-landing` is not installed.
- `src/rs/multiagent/`: offline plan validation and deterministic serial DAG runs in disposable worktrees, durable run and decision state, local-inference proposals with independent model review, edit application, read-only sandbox tools and verification manifests. The `hermes-harness multiagent` subcommands are a diagnostic surface.
- `src/rs/deployment/`: provider registry and `provider-probe`, `model-evaluate`, host-local GPU policy with an explicit switch, and `deploy-preflight`.
- `src/rs/gateway/`: gateway attachment, session discovery, durable admission store, receipt resolution, read-only and asynchronous recovery, contract and lookup probes, Unix-socket qualification.
- `src/rs/supervised/`, `src/rs/agents/`, `src/rs/services/`, `src/rs/broker/`, `src/rs/sandbox/`, `src/rs/git/`: the supervised worker and reviewer pipeline as explicit steps with no automatic role chaining, systemd service control and journals, the capability broker, the Bubblewrap launch plan, worktree audits and handoff.
- `src/rs/operator/`, `src/rs/probe/`: read-only host and cross-ledger inspection, and the `hermes-probe` subcommands.
- `src/rs/ci/`, `src/rs/evaluation/`: CI observation and merge gates, and the scoped cyber evaluation.

Offline tests use scripted transports and deterministic doubles. They show that the contracts hold; host isolation and live model behavior stay unverified.

## Not qualified on a live host

- Installing or upgrading the harness next to a live Hermes gateway (`docs/deployment/gateway-host.md`, `docs/deployment-qualification.md`).
- A Hermes gateway session discovering a skill, running it, and returning evidence to the originating conversation.
- Authentication of the bridge `--session` value as the real current gateway session; it is checked for syntax only.
- Enforcement that coding tasks go through the harness instead of an unsupervised shell.
- An authenticated worker and reviewer run against a remote llama.cpp provider, full 65,536-token context, and Hermes terminal tool reliability.
- GPU switch, rollback and restart safety on real hardware, and a lock shared between hosts.
- Proof that a gateway-submitted worker runs inside the intended service and worktree, and that systemd memory, CPU and task limits are enforced under load.
- An authenticated broker bound to a verified worker or reviewer identity.
- A production-grade sandbox, and repair application with re-checkpoint and re-verification (`apply-reviewed` does not accept repair receipts).
- A primary coordinator model that plans and dispatches, local models doing tool-using edits, and qualified parallel workers.
- Integrated Git changes across DAG nodes: predecessor edits do not propagate and there is no integrated HEAD.
- The PR and CI endgame as a single workflow.

Live steps and acceptance checks: [docs/deployment/acceptance-tests.md](docs/deployment/acceptance-tests.md), [docs/deployment/provider-routing.md](docs/deployment/provider-routing.md), [docs/deployment/gpu-policy.md](docs/deployment/gpu-policy.md), [docs/deployment/sandbox-tools.md](docs/deployment/sandbox-tools.md). Operator usage: [docs/user-guide/hermes-workflows.md](docs/user-guide/hermes-workflows.md), [docs/user-guide/approval-recovery.md](docs/user-guide/approval-recovery.md). Offline runtime: [docs/multiagent-runtime.md](docs/multiagent-runtime.md).

## Trust boundaries

- A language model prompt is never enforcement for read-only or target scopes. Filesystem, process, network, working-tree and secrets policies are enforced outside models.
- An isolated Git worktree is not a filesystem sandbox. Post-change audits detect ownership violations after the fact; they do not prevent them.
- CLI approvals are not bound to an authenticated identity.
- A working Unix socket or broker does not show that gateway tool execution runs inside Bubblewrap or systemd isolation.
- Model inference does not authorize application, application does not authorize execution, and passing tests do not authorize merging.
- Uncertain outcomes are never retried automatically. Ledgers of uncertain submissions are kept, not deleted, so they cannot be replayed.

## Operational hygiene

- Never commit secrets, provider tokens, private Hermes configuration or captured prompts with sensitive content.
- Provider registries name the key variable (`key_env`); the key value lives only in the environment of the calling process, never in JSON, Git, logs or CLI arguments.
- Create state and configuration directories with mode 0700.
- Back up state with file modes intact, and keep per-run evidence outside the tracked tree.

## History

The Python original is by Seth Barrett; milestones M1 to M5 were that project's phases. Design notes for the subsystems are in [docs/design-notes.md](docs/design-notes.md), and porting rules in [docs/rust-port/CONVENTIONS.md](docs/rust-port/CONVENTIONS.md).
