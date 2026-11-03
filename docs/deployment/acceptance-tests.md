# Gateway host acceptance tests: stage gates and a disposable example

Instructions, not a record of a run: every command is verified offline against the source, and no gate has been passed on a live host. Do not modify the existing gateway, the `<gpu-host>` model units, the reverse proxy or DNS during qualification.

## Evidence record

For each attempt, record in a private evidence directory outside Git: date and time, approved harness commit, Hermes version, originating session identity, service UID and working directory, the exact action, selected model IDs, HTTP statuses without headers or secrets, Git HEAD, owned paths, receipt file names with SHA-256, the step result (`PASS`, `WARN`, `BLOCK` or `UNKNOWN`) and the rollback decision. A model claiming it ran a tool without a real terminal receipt is `BLOCK`.

| Gate | Minimal check | Pass requirement |
|---|---|---|
| G0 baseline | Host inventory, gateway active, backup and rollback path | No existing configuration changed |
| G1 install | Isolated pinned checkout, Python >=3.14, `cargo build --release --locked` | Checkout and pin verified as `hermes` |
| G2 preflight | `deploy-preflight` without network probes | Report names the real blockers; no gateway change |
| G3 providers | Separately authorized `provider-probe`, proxy 401, TLS, model ID | Expected identity and authenticated completion, no leaked key |
| G4 64K model | Real long-prompt token counters and a timed task | Measured context evidence, not a profile integer |
| G5 Hermes skill | A real Hermes session runs `/lets-investigate` | The originating session sees the real Git HEAD and terminal provenance |
| G6 sandbox | OS isolation checks from the service account | Home, secret and network isolation and the fixed test command proven |
| G7 supervised model | Approved disposable proposal, review and application | Source scope, independent review, immutable hashes, pending human decision |
| G8 functional | New execution grant, Bubblewrap | Fixed unittest command runs only after a verified checkpoint; results kept |
| G9 GPU | Contention and rollback trials on `<gpu-host>` | Pinned fallback stays available; `UNKNOWN` is never replayed |
| G10 recovery | Inspect evidence, restore staged skills only | No replay; gateway, proxy and DNS unchanged |

G2 is expected to return `ready=false`: `native_session`, `context_64k` and `tool_qualification` have no live evidence input. Do not weaken the checks to get a green result.

## A. Disposable Python repository (separate approval)

Operator diagnostics, not Hermes chat commands. Run as `hermes` in that account's own directories, with a real Hermes session ID for live trials or a clearly labeled diagnostic ID for shell-only trials. Use a new `ROOT` and `TASK` each time; every model call and every write or execute step needs its own grant. The fixture is a clean Git repository with one trusted test (`2+3 == 5`) and no `calculator.py`; creating it runs no tests and never overwrites an earlier `ROOT`:

```bash
sudo -u hermes -H /bin/bash <<'BASH'
ROOT=/home/hermes/projects/qualification-calculator-001
test -e "$ROOT" && { echo "ROOT exists: choose a new one; no files changed" >&2; exit 1; }
install -d -m 0700 "$ROOT" "$ROOT/repo" "$ROOT/repo/tests" "$ROOT/evidence"
cd "$ROOT/repo" && git init -b main
printf '%s\n' 'import unittest' 'from calculator import add' '' 'class CalculatorTest(unittest.TestCase):' \
  '    def test_add(self):' '        self.assertEqual(add(2, 3), 5)' > tests/test_calculator.py
git add tests/test_calculator.py
git -c user.name=HarnessFixture -c user.email=fixture@example.invalid commit -m "test: add trusted calculator acceptance case"
git rev-parse HEAD
BASH
```

The test file is trusted input, not worker output. The printed HEAD is the baseline for every later receipt.

## B. Read-only skill verification (G5)

From an isolated Hermes conversation, request:

> `/lets-investigate Inspect only Git metadata for /home/hermes/projects/qualification-calculator-001/repo. Do not read credentials, edit files or execute tests. Return the observed HEAD and branch.`

Confirm the installed Hermes version discovered the skill, read the real terminal trace (expected: `hermes-native lets-investigate --repo ...`) and match the HEAD to the fixture. Without a tool receipt from the originating session, G5 stays `BLOCK` even if the CLI printed JSON.

## C. Validate a bounded plan (no inference)

Hermes may propose a one-node plan but writes it only with consent:

```json
{"id": "qual-calculator-001", "objective": "Implement correct arithmetic addition in calculator.py",
 "nodes": [{"id": "add", "objective": "Create calculator.py with add(a,b)", "owned_paths": ["calculator.py"], "dependencies": []}]}
```

Validate it with `/game-plan` or `hermes-native game-plan --plan /ABS/PATH/plan.json`. Validation is not model dispatch, and `/agents-assemble` defaults to the offline `DemoFactory`, so running it is no evidence of a local model proposal.

## D. Proposal, review, application and functional check (one grant per command)

`live-review` needs an approved local worker and an independent reviewer on loopback Ollama, an existing private evidence directory and a clean Git baseline. Remote llama.cpp is not wired into `live-review`; do not assume an Ollama service exists on the gateway host. Run each command only after approval of that exact step:

```bash
BRIDGE=/home/hermes/projects/hermes-harness-qualification/target/release/hermes-skill-bridge
ROOT=/home/hermes/projects/qualification-calculator-001 TASK=qual-calc-unique-task SESSION=ACTUAL_HERMES_SESSION
cd /home/hermes
"$BRIDGE" live-review --repo "$ROOT/repo" --state "$ROOT/evidence" \
  --task-id "$TASK" --objective "Implement calculator.py add(a,b) as a+b; tests are read-only" \
  --owned-path calculator.py --session "$SESSION" --authorize-inference
"$BRIDGE" apply-reviewed --repo "$ROOT/repo" --receipt "$ROOT/evidence/$TASK.json" \
  --workspace "$ROOT/worktree-$TASK" --session "$SESSION" --authorize-apply
"$BRIDGE" verify-applied --repo "$ROOT/repo" --receipt "$ROOT/evidence/$TASK.json" \
  --workspace "$ROOT/worktree-$TASK" --evidence "$ROOT/evidence/$TASK-verified.json" \
  --session "$SESSION" --authorize-verify
"$BRIDGE" checkpoint --receipt "$ROOT/evidence/$TASK.json" \
  --verification "$ROOT/evidence/$TASK-verified.json" \
  --evidence "$ROOT/evidence/$TASK-checkpoint.json" --session "$SESSION" --authorize-record
"$BRIDGE" functional-verify --checkpoint "$ROOT/evidence/$TASK-checkpoint.json" \
  --workspace "$ROOT/worktree-$TASK" --evidence "$ROOT/evidence/$TASK-functional.json" \
  --session "$SESSION" --authorize-execution
```

- After `live-review`, inspect `$ROOT/evidence/$TASK.json`: source hash, review decision, evidence SHA-256. A rejection, timeout or missing evidence stops the path; a task ID is never redispatched.
- Apply only an accepted review. `apply-reviewed` creates a new disposable worktree; never point it at the source checkout or an existing worktree. Confirm Git HEAD, paths and hashes before each step. A checkpoint does not show that tests passed.
- `functional-verify` needs qualified Bubblewrap confinement and a human approval. The command is fixed (`python3 -m unittest discover -s tests -p test_*.py`); the model cannot choose it. A PASS or a durable failure receipt grants no commit, push or merge. A failure can enter the bounded repair sequence, which records one-shot review proposals only; never pass a repair review to `apply-reviewed`.

On any unknown outcome, stop and use the read-only `repair-inspect` or `inspect`. Record task ID, Git baseline, receipt SHA-256, reviewer verdict, sandbox result, open blockers and the pending operator decision. There is no `/baton-pass` skill and no automatic Git integration or PR creation.

## Negative tests before calling the host qualified

- From an unauthorized client the proxy denies the API; from `<harness-host>`, unauthenticated requests get `401` and authenticated ones return the model ID without printing tokens. Backend ports 8080 and 8081 stay unreachable from unapproved clients.
- A missing `LOCAL_LLM_API_KEY`, bad TLS trust, wrong model ID, a 4K model posing as 64K and an offline server each give a specific `BLOCK` or error, never cloud fallback.
- The real `hermes` UID and working directory, no secret reads or writes, network denial in Bubblewrap, no unauthorized worktree paths, and denial of generic-shell bypass for coding tasks.
- GPU admission with a pinned fallback, overlapping requests, the wrong host and an unknown switch journal. Do not stop running llama.cpp services outside approved maintenance.
- The gateway stays active before and after every experiment; DNS policy stays unchanged.

See [user workflows](../user-guide/hermes-workflows.md) and [recovery](../user-guide/approval-recovery.md).
