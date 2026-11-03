# Approval boundaries, receipts, recovery and handoff

Each gate below is tested offline; none is qualified from a live Hermes session. A skill instruction, model response, reviewer `accept`, passing test or JSON `ok=true` is not a human approval. Each grant needs a verified originating Hermes session, the exact repository baseline, owned paths, the action and durable evidence.

| `hermes-skill-bridge` command | Separate grant | Result | Never implies |
|---|---|---|---|
| `live-review` | `--authorize-inference` | Local proposal and independent reviewer receipt | Worktree application |
| `apply-reviewed` | `--authorize-apply` | Audited new disposable worktree | Test execution |
| `verify-applied` | `--authorize-verify` | File-integrity evidence | Functional pass |
| `checkpoint` | `--authorize-record` | Linked integrity checkpoint | Execution grant |
| `functional-verify` | `--authorize-execution` | Fixed Bubblewrap unittest receipt | Merge or human acceptance |
| `repair-open` | `--authorize-record` | Repair sequence linked to the failed test | Model call |
| `repair-reserve` | `--authorize-reserve` | One bounded attempt, at most 3 | Dispatch approval |
| `repair-dispatch` | `--authorize-inference` | One-shot replacement inference and independent review | Repair application or execution |
| `repair-disposition` | `--authorize-record` | Operator decision evidence | New worktree or re-run |

These are bridge commands, not Hermes chat slash commands. `apply-reviewed` does not accept repair-review receipts, and there is no repair-specific apply, checkpoint or re-verification path yet.

## One-shot recovery

1. Inspect the state folder, Git HEAD and ownership scope. Do not change receipts, source worktrees or the model server while the outcome is uncertain.
2. A dispatch claim without a final review is `UNKNOWN`; never retry its task ID. This covers network timeouts and incomplete server-side cancellation, where a retry could repeat inference or effects.
3. Read state with `hermes-skill-bridge inspect --state STATE --run-id RUN --session REAL_SESSION` (run ledger) and `hermes-skill-bridge repair-inspect --state STATE --task-id TASK --session REAL_SESSION` (repair attempts). Both are read-only.
4. Confirm receipt paths and SHA-256 hashes. A replay refusal such as `File exists` or `already claimed` is the safety gate working, not permission to delete the claim.
5. Return observed facts, pending human decisions, unresolved state and the next authorized action to the same Hermes session as a plain summary; there is no `/baton-pass` skill.

## What not to do

- Never use `--yolo`, blanket operator approval, `sudo` delegation to model code, host shell bypass, untrusted repository hooks or unattended merge or push.
- Never put `LOCAL_LLM_API_KEY`, gateway credentials, messaging tokens, PATs or passwords in evidence or prompts. Redact logs; do not weaken TLS validation.
- Never reuse task IDs, force-remove `*-dispatch.json`, overwrite review evidence or clear a GPU `UNKNOWN` journal to get a fresh attempt.
- Keep failed Bubblewrap receipts and the original checkpoint when a later model proposes a fix.

See [acceptance tests](../deployment/acceptance-tests.md).
