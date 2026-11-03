# Offline multi-task coordinator

`hermes-harness multiagent` runs a task DAG on top of `git::worktree_delegation` and
`supervised::offline_orchestrator`; the code is in `src/rs/multiagent/`. Each node gets
its own disposable Git worktree, immutable dispatch fence, verified Git audit, distinct
reviewer identity, receipt and pre-approval request. Nodes run serially, nothing is
merged, and every run ends at an immutable pending human decision. Workers and reviewers
are injected offline doubles; no OS-level read-only confinement of the reviewer is
claimed. Package invariants are in [design-notes.md](design-notes.md#multiagent).

## Run

```bash
hermes-harness multiagent demo
hermes-harness multiagent doctor
hermes-harness multiagent plan docs/multiagent-example.json
# Use only a clean disposable Git repository; the worker is a deterministic offline double.
hermes-harness multiagent --state /tmp/harness-state run-offline docs/multiagent-example.json --repo /tmp/disposable-repo
hermes-harness multiagent --state /tmp/harness-state status example
hermes-harness multiagent --state /tmp/harness-state decision example reject
```

The default state directory is `.harness-multiagent`. `status`, `inspect` and
`reconcile` are read-only. `cancel` reports that no background process exists and keeps
the evidence. A decision (`approve`, `reject`, `request-changes`) is an immutable record
and does not authorize applying, merging or pushing code. A partly failed run stays
fenced; there is no automatic recovery, replay or resubmission.

## Limits

- No live Hermes gateway dispatch, provider credentials, tool execution or
  broker-backed requests.
- No parallel execution or automatic DAG integration. A dependency orders tasks and
  does not copy predecessor edits, so a human integrates the results.
- No production sandbox. The Git ownership audit detects changes after the fact and does
  not isolate files or processes. Never inject untrusted live agents, and do not replace
  `DemoFactory` with a tool-capable provider until execution isolation is demonstrated.
- Incomplete worktrees and state are inspected by hand before removal; uncertain
  evidence is never cleaned up automatically.
