# Using the harness inside Hermes

The entry point is a Hermes chat session with the harness `SKILL.md` files loaded. The skills call `hermes-skill-bridge` and `hermes-native`; operators use those directly only for diagnostics. Hermes decides whether to invoke a skill and its terminal tool. The harness registers no slash commands and does not intercept generic shell tasks.

The skill files and every backend command are tested offline, and a disposable Ollama worker, reviewer and Bubblewrap fixture has run through direct bridge calls. Not qualified on a live host: gateway skill discovery, originating-session binding, live llama.cpp dispatch and enforcement that coding tasks go through the harness.

## Skills and what they can show

| Skill | Can currently demonstrate |
|---|---|
| `/lets-investigate` | Read-only Git metadata via `hermes-native lets-investigate`; no semantic file analysis |
| `/game-plan` | Validates an operator-approved JSON plan; no model generates it |
| `/agents-assemble` | Default is `hermes-native agents-assemble --confirm-offline` with deterministic doubles; `hermes-skill-bridge live-review` and `assemble-live` run real local inference on opt-in |
| `/what-we-know` | Read-only state inspection through `hermes-skill-bridge inspect` |
| `/cyber-warrior` | Scoped defensive, read-only security guidance; no qualified live security executor |
| `/stick-the-landing` | Skill file exists but `hermes-skill-install` does not install it |
| `/baton-pass`, `/crashout`, `/review-circus` | No skill file |

A name in a `SKILL.md` shows what the repository contains, not what Hermes has registered; check discovery on the installed version. No backend result grants commit, push, merge or deployment.

## Example operator conversation

Use a disposable Hermes session and a Git path you are allowed to use. Do not accept a claim that a command ran unless its terminal receipt shows it.

> Operator: `/lets-investigate Inspect the Git metadata for /home/hermes/projects/demo-calculator/repo. Do not modify or run repository code. Report the HEAD and changes actually observed.`
>
> Operator: `/game-plan Propose a bounded one-file implementation plan for calculator.py; tests/test_calculator.py is read-only. Show the plan to me before writing any plan.json.`
>
> Operator: `/agents-assemble Do not run the offline simulator as if it were a real worker. I authorize only a proposal and review for calculator.py in the disposable repository, after you confirm its baseline and the exact local models. Do not apply the proposal or run generated tests.`

The last message states intent and grants nothing. The skill must ask for a specific inference approval and call `live-review` with the original session ID before claiming any worker activity. If the installed skill only offers offline simulation, it stops and says so.

After an accepted review, the operator approves each transition separately:

> Operator: `Show the immutable proposal and reviewer receipt and its SHA-256. I will decide whether the reviewed change can be applied to a fresh disposable worktree. Do not execute tests yet.`
>
> Operator: `I approve applying only receipt PATH to new worktree PATH at the exact audited Git HEAD. Return the apply receipt; do not run tests.`
>
> Operator: `I approve a separate read-only integrity check, then an immutable checkpoint. Do not execute generated code until I explicitly approve Bubblewrap functional verification.`
>
> Operator: `I approve the recorded checkpoint for the fixed, network-isolated unittest verification only. Stop and return the result to this Hermes session. Do not commit, push or merge.`

A failure or ambiguous dispatch pauses for human inspection and is never retried automatically. Full commands are in [acceptance tests](../deployment/acceptance-tests.md) and [approval and recovery](approval-recovery.md).

## First live skill test

Follow [gateway host install](../deployment/gateway-host.md): install only the reviewed `lets-investigate` skill after approval, list skills with the installed Hermes version and invoke the skill from a real gateway session. Capture the session ID, user identity, tool command, returned Git HEAD and the chat response. If any link is missing, mark it `BLOCK`. Never type `SESSION_ID` literally as the session.

## Offline simulation and real inference

```text
Offline:     Hermes skill -> hermes-native agents-assemble --confirm-offline
             -> canonical DAG + deterministic DemoFactory workers
             -> pending human decision (no model inference)
Live opt-in: Hermes skill -> hermes-skill-bridge live-review
             -> authorized local worker + independent reviewer
             -> immutable evidence (no application or execution)
Next gates:  apply-reviewed -> verify-applied -> checkpoint
             -> functional-verify (Bubblewrap; new grant)
             -> operator decision (no automatic merge)
Repair:      failed functional receipt -> repair-open -> repair-reserve
             -> repair-dispatch -> repair-inspect / repair-disposition
             -> proposal only; repair-specific apply and re-verification not implemented
```

Most of this pipeline has run as direct diagnostic bridge calls, not as one qualified end-to-end chat flow. Say which one you mean.
