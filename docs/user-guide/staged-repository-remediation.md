# Staged Dependabot and Trivy remediation through Hermes

The stage ledger commands are tested offline; this four-stage flow has not been run end to end from a live Hermes session. The intended operator request, from a real Hermes session, looks like:

```
/lets-investigate check out dependabot prs and trivy scan fails on <frontend-repo> and develop fixes
```

Hermes keeps one originating session and asks for explicit approval at every boundary:

1. **lets-investigate**: Resolve the exact repository and its `dev` ref using the operator's approved GitHub credential helper. Ask approval to clone or fetch. Inspect open Dependabot PRs, base branches, CI, dependency manifests, Trivy reports and failing Actions. Where permitted, run read-only scans with trusted, approved tooling. Report real PR numbers, findings and the Git HEAD, submit the evidence to the ledger and stop to ask approval for `what-we-know`.
2. **what-we-know**: Reconcile the evidence with existing receipts. Separate verified findings from missing scans, proposed changes, conflicting Dependabot PRs and unknowns. Prepare a specific plan, submit evidence and stop to ask approval for `agents-assemble`.
3. **agents-assemble**: With an approved plan and separate inference and tool grants, run `assemble-live`. It handles one coding task against existing tracked UTF-8 files of at most 8192 bytes each; its sandbox tools are read-only, and accepted replacements need a separate application approval for a disposable worktree. If the job exceeds those limits, report it and stop; never claim a full remediation ran. Check the actual diff and any separately authorized tests or scans, submit evidence and stop to ask approval for `stick-the-landing`.
4. **stick-the-landing**: Inspect the approved diff, Git baseline, tests, Trivy output and affected Dependabot PRs. Present the target repository, a feature branch based on `dev`, the PR title and body and the exact actions. Ask separately for commit, push, PR creation and any close or merge. Act only after each grant, using the approved identity, then submit the final PR evidence and ask for final confirmation.

## Stage ledger commands

`workflow-begin` creates a run pinned to the original repository and HEAD. `workflow-submit` binds a stage evidence file to its SHA-256 digest. `workflow-approve` advances only with the matching digest and `--operator-confirmed`. `workflow-status` is read-only. Stages run in strict order. All four are `hermes-skill-bridge` subcommands:

```sh
# Once, after permission for the initial read-only inspection
hermes-skill-bridge workflow-begin \
  --state /ABS/PRIVATE-STATE --run-id UNIQUE-ID --repo /ABS/CLEAN-REPO --session ACTUAL-SESSION

# Stop and show the operator the investigation and its digest
hermes-skill-bridge workflow-submit \
  --state /ABS/PRIVATE-STATE --run-id UNIQUE-ID --stage lets-investigate \
  --evidence /ABS/INVESTIGATION.json --session ACTUAL-SESSION

# Only after the operator consents to that exact evidence
hermes-skill-bridge workflow-approve \
  --state /ABS/PRIVATE-STATE --run-id UNIQUE-ID --stage lets-investigate \
  --evidence-sha256 ACTUAL-SHA256 --session ACTUAL-SESSION --operator-confirmed
```

Repeat submit and approve for `what-we-know`, `agents-assemble` and `stick-the-landing`. The flag does not prove consent: Hermes must confirm authorization in the conversation and keep the provenance, and never use a model-generated approval string. Deny forgotten grants, duplicate receipts, unknown outcomes, untrusted code execution and reuse across repositories.

The credential helper only selects a PAT; it is not a repository-scoped credential broker. Never expose it or its token files to sandboxed local workers.

The ledger records stages and approvals; it does not make an autonomous remediation engine. End-to-end task execution and host confinement must be qualified before production use.
