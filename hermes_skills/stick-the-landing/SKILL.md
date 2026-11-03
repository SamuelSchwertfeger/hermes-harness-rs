---
name: stick-the-landing
description: Verify reviewed changes and gate PR creation on approval.
version: 0.1.0
platforms: [linux]
author: Hermes Harness
license: MIT
metadata:
  hermes:
    tags: [coding, verification, github]
    category: development
---

# Stick The Landing

Build first: `cargo build --release --locked` in the harness repo.

Use after an operator-approved `agents-assemble` result on the exact target repository, following `docs/user-guide/staged-repository-remediation.md`.

Validate the original repository and `dev` branch, review the exact worktree diff, checks and Trivy findings, and summarize any Dependabot PRs this would supersede. Never invent test, PR or scan results.

Stop and ask the operator to confirm the feature branch, commit, push and PR creation. Only then may the trusted Hermes coordinator, never a sandboxed worker, open a PR against `dev` using the operator's approved GitHub credential helper. Merging, closing or superseding Dependabot PRs needs its own confirmation.

Record a final evidence report with `workflow-submit` for stage `stick-the-landing`, show its SHA-256 and ask for final approval before `workflow-approve`. If there are no changes or validation is incomplete, report the blockers; never produce a fictitious PR.

## When to Use

Use when an approved repository repair is ready for validated PR creation.

## Pitfalls

A reviewer verdict is not permission to push. An offline run is not a real patch. A ledger flag does not authenticate the human.

## Verification

Report the exact commit and branch, PR URL, target `dev`, checks, scan outcomes and immutable approval evidence.

## Optional `<inference-host>` model pool

With an inference grant, list the models installed on `<inference-host>` (reached through the loopback endpoint `127.0.0.1:11435`): `/ABS/PATH/TO/HARNESS/target/release/hermes-skill-bridge xps-models --session ACTUAL_SESSION`. Pick any returned model that suits the request and run `hermes-skill-bridge xps-infer --session ACTUAL_SESSION --model EXACT_NAME --prompt 'BOUNDED_APPROVED_TASK' --authorize-inference`. This is one-shot inference, not shell access, and it does not replace approval-gated file changes. Never download a model or report a missing one as available. If `<inference-host>` is offline, say so; the Hermes coordinator's own provider and fallback are unaffected.
