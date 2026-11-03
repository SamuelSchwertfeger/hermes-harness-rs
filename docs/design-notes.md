# Design notes

Invariants carried over from the Python project's milestone notes (M1 to M5); the code is the source
of truth. `README.md` covers build and run, `DIVERGENCES.md` the differences from the Python,
`docs/user-guide/` and `docs/deployment/` operation, and `docs/rust-port/REMOVED-SCRIPTS.md` maps
removed scripts to `hermes-probe` subcommands.

Three rules hold in every package. An ambiguous remote outcome is recorded and never retried,
resubmitted or replayed; an operator inspects the ledgers. Gateway status strings are opaque and
never prove that work completed or stopped. No module approves, commits, pushes or merges.

## core

Path: `src/rs/core/`.

- `core::watchdog::supervise` runs a trusted argv in a new process group under `SupervisionOptions`
  and a `Budget` deadline, captures output to per-run files, sends TERM then KILL to the group and
  records a JSON outcome. It does not cancel gateway-owned work. An unreaped zombie can yield a
  conservative `cleanup_failed`. No log quotas, cgroup containment or cancellation across harness
  crashes. `core::execution` applies the same bound with a 30-second default.
- `core::transport` runs a caller-supplied absolute executable (`ForegroundOptions`) for the offline
  worker protocol and checks exit status, group liveness, UTF-8, non-emptiness and byte budget.
  Children are not sandboxed, so it must not run Hermes, Codex, Ollama, shells or plugins.
- `core::delegation` (`begin`, `delegate`, `review`, `request_approval`, `phase`) writes each
  stage's evidence without overwrite, binds the brief digest, requires distinct worker and reviewer
  identities and ends with `approved: false`. `OfflineAgent` implementations must be deterministic
  doubles; model-reported identity and `owned_paths` are not security boundaries.
- `core::task_reservation::reserve` takes an interprocess `flock` and persists task ID, brief
  digest, paths, source HEAD and branch before any worktree exists; later stages write irreversible
  preparing, prepared and dispatch fences. `reconcile` is read-only and returns `reserved`,
  `preparing_uncertain`, `prepared`, `dispatch_uncertain`, `worker_recorded` or
  `manual_intervention`; no state authorizes retry or cleanup. Locks do not cover a hostile process
  under the same account.
- `core::reconciliation` accepts only `outcome_unknown` records and an injected resolver. A found
  receipt is identity-checked and persisted; not-found and indeterminate leave the record unknown,
  since a miss does not prove non-admission.
- `core::routing::select` refuses disabled routes and the cyber role without an authorized target; a
  model choice grants no privileges. `core::state` guards ticket transitions and never
  auto-advances. `core::ownership` confines changed paths to the approved set and refuses symlink
  crossings. `core::ollama` sends single-lane, tool-free requests under an advisory GPU lock.

## git

Path: `src/rs/git/`.

- `git::worktrees::create_worktree` requires a clean source and creates `harness/<ticket-id>` in a
  new directory. `audit_worktree` checks committed, staged, unstaged and untracked changes against
  the ownership prefixes and rejects symlinked changes. `remove_worktree` refuses dirty or
  unexpected branches and keeps the branch.
- The audit is after the fact and is no isolation boundary: an agent with a shell can write the
  source repository, Git metadata or anything outside the worktree. Containment needs OS permissions
  or mounts.
- `git::worktree_delegation`: `prepare_worktree` records the baseline and task evidence;
  `delegate_in_worktree` fences dispatch, calls the worker once and saves the audit with SHA-256
  hashes of audited files; `review_worktree` and `request_worktree_approval` re-verify through
  `verified_audit` first. A failure after dispatch is uncertain, with no retry or automatic
  deletion; a crash before the ledger write leaves an orphaned worktree on purpose.

## gateway

Path: `src/rs/gateway/`. Every module takes injected, already-authenticated RPC objects. None starts
or stops a gateway, loads credentials or calls `session.resume`. The production connector fails
closed (see `DIVERGENCES.md`).

- `submission_id` is the stable harness-generated request ID; `admission_id` is assigned by Hermes.
  `prompt.receipt` and `prompt.cancel` take the admission ID, and the client cannot invent a lost
  one.
- `gateway::gateway_lifecycle` is a simulator; a `LifecycleConfig` with `simulated_only` false is
  rejected. A cancel response yields `cancel_requested`, not cancelled.
- `gateway::gateway_identity` requires a nonblank `admission_id` and `status` and matching identity
  on every response; a dropped connection is an outcome-unknown error.
  `OfflineAdmissionClient::cancel_queued` refuses without an admission ID and treats timeout as
  unknown.
- `gateway::admission_store::AdmissionStore` is a SQLite ledger keyed by `submission_id`, with
  `local_state` `prepared`, `outcome_unknown`, `admitted` or `receipt_observed`. Call `prepare`
  before any remote call, `mark_outcome_unknown` on transport error and `record_receipt` after a
  validated receipt. Duplicates, admission ID changes and symlinked paths are refused; the file is
  0600. Keep it on a local filesystem.
- `gateway::gateway_session_creation::prepare_disposable_session` writes an exclusive 0600 creation
  record before its one `session.create`. The record, even partial, is a no-retry barrier. A new
  connection does not inherit the subscription, so creation and submission share one connection.
- `gateway::gateway_disposable_admission::create_and_submit_once` composes that with
  `gateway::gateway_live_admission::submit_once` on one RPC object: mark creation unknown before
  `session.create` and admission unknown before `prompt.submit`, then persist the admission ID
  before the single `prompt.receipt`. Creation passes an empty toolset. Existing stored sessions are
  never disposable targets.
- `gateway::gateway_event_replay` and `gateway::gateway_pending::inspect_pending` find a candidate
  in `session.info` events. Replay calls `session.events.since` twice, first for the epoch and
  watermark, then from zero in that epoch; truncation, epoch change or malformed events are
  inconclusive. A candidate needs exactly one `pending` item whose `input_id` equals the submission
  ID.
- `gateway::gateway_recovery::recover_unknown_admission` and
  `gateway::gateway_async_recovery::recover_with_async_rpc` persist a candidate only after
  `prompt.receipt` (via `gateway::gateway_resolver`) returns the same ID; inconclusive evidence
  leaves `outcome_unknown`. Absence from the replay never proves non-admission.
- `gateway::gateway_attachment::attach_existing_gateway` polls read-only discovery for at most 15
  seconds at 0.5-second intervals, retrying only `absent` and `starting`, then connects once;
  anything else fails closed. `gateway::gateway_live_recovery::recover_with_existing_gateway`
  attaches and recovers.
- `gateway::gateway_readonly::read_from_hermes` exposes only `session.info`.
  `gateway::gateway_session_discovery` sends one bounded `session.list` and never selects or creates
  a session.
- `gateway::gateway_contract_validation::validate_existing_gateway` sends `ping`, `session.list`
  and, for a known session, `session.info` and two epoch-bound `session.events.since` calls. It
  rejects epoch changes, sequence regressions and malformed envelopes. `replay_consistent` is
  metadata validation; raw events are never printed or stored.
- `gateway::gateway_terminal_reconciliation::reconcile_terminal_once` reads terminal receipts for
  known admissions. `gateway::supervised_gateway_receipts::reconcile_supervised_roles_once` does so
  for worker and reviewer in separate sessions and fails closed on missing admissions, wrong task or
  identical sessions. Receipts do not show that the reviewer read the worker's changes.
- `gateway::gateway_contract_probe` and `gateway::gateway_lookup_probe` statically scan Hermes
  sources within bounds and always report `verified: false` or `capability_verified: false`.
  `gateway::gateway_socket_qualification` checks only that a `bwrap --unshare-net` process can reach
  a mounted Unix socket.
- `gateway::hermes_adapter::execute` always fails closed, because killing a foreground Hermes
  process did not stop gateway-owned jobs.
  `gateway::scripted_supervised_gateway::ScriptedSupervisedGateway` is the network-free double
  behind `hermes-supervised-demo`.

## broker

Path: `src/rs/broker/`.

- `broker::live_capability_broker`: the operator records a `BoundRequest` (task, role, session,
  submission, prompt SHA-256) and `CapabilityLedger::issue` returns a one-use token that never
  travels in argv or logs. The broker checks peer UID and exact binding, records
  `submitted_uncertain` before calling its adapter and never accepts an agent-chosen destination or
  method. A denial after reservation consumes the token.
- Peer UID does not separate two sandboxes under one account; distinct socket mounts and
  per-invocation tokens are still required.
- `broker::live_broker_admission`: trusted host code calls `OperatorGrantStore::authorize` with a
  human-confirmed flag for an exact binding and prompt digest. The flag is no authentication, so the
  call must never be reachable from a sandbox. `QualifiedAdmissionAdapter` checks binding, systemd
  state and prompt digest and consumes the grant before the one-shot admission path.
- `broker::live_broker_listener` refuses existing socket paths and world-writable parents, bounds
  message size, serves a finite number of connections and unlinks its socket on stop.
- The capability ledger and the admission store are not one transaction; an ambiguous result in
  either ends that submission.

## services

Path: `src/rs/services/`.

- `services::live_service_journal::ServiceJournal` writes `launch_uncertain` before any subprocess
  and refuses a second launch of the unit. A stop records `stop_uncertain` and becomes
  `stopped_observed` only after verified inactivity.
- `services::live_service_controller::ServiceController` takes an injected
  `ServiceCommandTransport`. `reconcile_stopped` only asks whether the unit is inactive and never
  sends a stop.
- `services::live_systemd_transport::RestrictedSystemdTransport` admits only the launch and stop
  argv of a bound `SandboxPlan` plus read-only inspection. `operator_subprocess_runner` uses no
  shell or sudo and a 10-second timeout, which counts as an unknown outcome.
- `services::live_runtime_readiness::assess_readiness` reports operator-claimed host controls and
  authorizes nothing.

## supervised

Path: `src/rs/supervised/`.

- `supervised::orchestration::dispatch_offline` composes ticket stages and private artifacts under
  `.harness-runtime/tickets/<id>/runs/<uuid>/`. It has no deadline or containment and takes only
  deterministic doubles.
- `supervised::offline_orchestrator::run_offline_task` runs one fresh task with `OfflineAgents` from
  reservation to an unapproved request; an existing task ID fails instead of resuming.
  `inspect_offline_task` only reads.
- `supervised::live_execution_coordinator::LiveExecutionCoordinator` stores one single-use
  `ExecutionBinding` per role, then launches. `submit_worker` and `submit_reviewer` need the exact
  binding and a fresh systemd observation, so a stale `running_observed` cannot authorize work.
- `supervised::live_reviewer_verdict` requires exactly `task_id`, `worker_submission_id`,
  `audit_sha256`, `decision` (`accept`, `changes_requested`, `reject`) and `findings`. A gateway
  `completed` status is no verdict.
- `supervised::live_human_decision::request_human_decision_once` writes
  `human-decision-request.json` exclusively with `approved: false`; a second call is refused.
- `supervised::live_supervised_recovery::RecoveryLedger` checkpoints `planned`, `worker_admitted`,
  `worker_audited`, `reviewer_admitted` and `pending_human`, each needing an evidence digest; a
  denied or uncertain step freezes at `halted`.
- `supervised::live_supervised_operator_flow` splits a run into separately authorized steps
  (`launch_worker` through `finalize`); none calls another, `status` only reads and `halt` does not
  stop a service.
- `supervised::live_supervised_finalization::finalize_existing_roles_once` reads only
  `prompt.receipt`, verifies the audit before and after, and recovers an existing decision request
  instead of overwriting it.

## agents

Path: `src/rs/agents/`.

- `agents::supervised_agents::prepare_supervised_plan` builds disabled worker and reviewer
  manifests; `SupervisedPlan::require_executable` always refuses.
  `agents::supervised_rehearsal::run_supervised_rehearsal` cannot enable execution.
- `agents::supervised_review_handoff::prepare_reviewer_handoff_once` combines an admission, a
  terminal receipt and `verified_audit` into a `ReviewerHandoff`; `verify_reviewer_handoff`
  re-checks before submission.
- `agents::supervised_reviewer_dispatch::submit_reviewer_once` re-verifies the audit around a
  `RuntimeQualifier`; the default `DisabledRuntimeQualifier` rejects. Concurrent workspace mutation
  stays a TOCTOU risk.
- `agents::supervised_execution` drives `hermes-supervised-demo` with offline doubles.
  `agents::supervised_status` (`hermes-supervised-status`) reads retained evidence.

## operator

Path: `src/rs/operator/`.

- `operator::live_operator_reconciliation` (`hermes-live-operator`) joins the binding, service and
  admission databases read-only. Every role carries `live_process_not_verified`, and `safe_to_retry`
  and `approved` are always false.
- `operator::live_operator_safety` refuses a reviewer with any writable bind, missing `/workspace`
  or `/evidence` sources and missing isolation flags.
- `operator::live_host_probe` (`hermes-live-host-probe --unit <unit> --execute-readonly`) compares a
  unit's properties with the sandbox limits and always prints `Gateway authorization: NOT GRANTED`.

## probe

Path: `src/rs/probe/`, the `hermes-probe` binary.

- `disposable-admission` needs `--execute`, `--confirm-disposable` and a matching
  `--confirm-create-request-id`, and refuses existing `--creation-record` and `--ledger` paths.
  `live-admission` needs `--submit`, `--confirm-session-id` equal to `--session-id`, and
  `--confirm-disposable-session`.
- `validate-existing` runs the contract validator; `--list-sessions` (`stored` or `live`, `--limit`
  1 to 20) excludes `--session-id`.
- `unknown-recovery` uses a random, never-submitted ID and expects it to stay unknown.

## sandbox

Path: `src/rs/sandbox/`.

- `sandbox::live_sandbox_plan::build_sandbox_plan` returns a `SandboxPlan` argv for `systemd-run`
  around `bwrap` and runs nothing. The worker workspace is writable, the reviewer's read-only;
  secrets are not mounted and the network is unshared. Limits: 512M memory, 50% CPU, 32 tasks.
  Enforcement under load is unmeasured.

## multiagent

Path: `src/rs/multiagent/`. Commands are in `docs/multiagent-runtime.md`.

- `multiagent::runtime::execute` runs a DAG serially with one worktree, fence, audit and reviewer
  per `Node`, ending at a pending human decision. `DemoFactory` supplies offline doubles.
- `multiagent::cyber`: the `cyber-warrior` role needs a `CyberGrant` in
  `ExecutionConfig::cyber_grant`, bound to repository, tasks, paths and approver. Live cyber
  dispatch stays refused.
- `multiagent::local_inference::LocalEndpoint` names a loopback chat-completions URL. `propose_plan`
  and `propose_edits` output is untrusted data, parsed by `multiagent::plan_io::parse_plan_json` and
  `multiagent::model_edits::parse_edit_proposal`. Non-loopback hosts, proxies and redirects are
  refused.
- `multiagent::apply_edits::apply_new_files` requires `authorized` and a clean worktree at the
  baseline, creates only new UTF-8 files and audits the changed set.
  `multiagent::evidence::review_bundle` is read-only evidence, not a review.
- `multiagent::routing::OllamaRouting` defaults to `qwen2.5-coder:7b`, `qwen2.5-coder:1.5b` and
  `qwen3:4b` (reviewer). `qualify_models` refuses missing models and a reviewer equal to the worker;
  `max_parallel` must be 1.

## native

Path: `src/rs/native/`.

- `native::workflows` (`propose_local_plan`, `propose_local_edits`, `apply_local_new_file_proposal`,
  `qualify_local_roles`) adds explicit authorization and Hermes session correlation.
- `native::repair_controller` backs `hermes-skill-bridge repair-open`, `repair-reserve`,
  `repair-inspect` and `repair-disposition`. Only confirmed ordinary test failures open; at most 3
  reservations; only `rejected` and `failed` allow another. Nothing permits apply, commit or push.
- `native::repair_dispatch` (`repair-dispatch`) writes an `O_EXCL` claim before loopback inference
  and limits replacements to checkpoint-owned paths. Nothing generated is applied or run.
- `native::skill_install` stages a `hermes-harness` category and never overwrites. An installed
  skill is instructions, not a tool.

## ci

Path: `src/rs/ci/`.

- `ci::ci_request::create_ci_request` records `ci-request.json` once after an operator-made commit
  and rejects dirty trees, wrong branches and scope drift. `inspect_ci_request` re-checks without
  mutation.
- `ci::ci_observation::evaluate_ci_observation` checks observations against a `CIRequest` and
  `CIPolicy`, rejecting commit mismatches, forks, other workflows and failures; pending stays
  incomplete.
- `ci::github_ci_reader::fetch_github_ci_evidence` uses only GET, up to 20 pages of 100; gaps and
  mismatches fail closed. Tokens never enter task records.
- `ci::ci_gate::assess_github_ci` returns `observed_pass_not_approval`, `incomplete`, `rejected` or
  `unknown`; none is an approval. Branch protection and workflow trust are not verified.

## evaluation

Path: `src/rs/evaluation/`.

- `evaluation::cyber_evaluation` scores pre-collected, tool-free observations against an immutable
  suite with deterministic expected and forbidden terms, and writes an exclusive 0600 summary with
  the suite SHA-256. Substring scoring is a regression fixture, not a measure of model safety.
