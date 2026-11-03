# Gateway host and GPU deployment: qualification contract

Step order: [gateway host install](deployment/gateway-host.md), [provider routing](deployment/provider-routing.md), [GPU policy](deployment/gpu-policy.md), [Hermes workflows](user-guide/hermes-workflows.md), [acceptance tests](deployment/acceptance-tests.md). Everything here is covered by offline tests and fixtures only; nothing is qualified on `<harness-host>` or `<gpu-host>`. No step approves unattended code execution, systemd takeover, merge or gateway changes.

## Installation without disturbing the gateway

1. Start from a clean checkout on the workstation, review the diff and run `make check`. Never copy an archive into an operational checkout or overwrite uncommitted files.
2. On `<harness-host>`, install the reviewed commit into a separate `hermes`-owned directory (for example `/home/hermes/projects/hermes-harness-qualification`) and run `cargo build --release --locked` there as `hermes`, with `/home/hermes/.local/bin` on `PATH`.
3. Leave the `hermes-gateway-<id>.service` unit, its messaging and provider settings and model aliases untouched. Run only `deploy-preflight` at first; it is read-only even with `--probe-network`.
4. Create separate private directories for sandbox worktrees, evidence and GPU lock files (directories `0700`, evidence `0600`). Never reuse earlier worktree or dispatch receipt paths.
5. Copy the example provider registry and GPU policy to private operator-owned locations and replace only the approved host, model, unit and VRAM fields with observed values. Never put API keys in JSON; `LOCAL_LLM_API_KEY` must exist in the `hermes` service environment. Verify the proxy credentials and local CA without disabling TLS verification.
6. Run the commands below through the Hermes `agents-assemble` skill's terminal with the active session ID. An operator shell is a diagnostic fallback and does not qualify the Hermes path.

```bash
# As hermes, from its own checkout and environment:
hermes-skill-bridge deploy-preflight \
  --workspace /home/hermes/projects/worktrees \
  --provider-registry /home/hermes/.config/hermes-harness/providers.json \
  --gpu-policy /home/hermes/.config/hermes-harness/gpu.json \
  --gateway-unit hermes-gateway-<id>.service \
  --session REAL_SESSION

# Optional: authenticated HTTPS requests to the proxy (/health, /v1/models, chat).
hermes-skill-bridge provider-probe \
  --provider-registry /home/hermes/.config/hermes-harness/providers.json \
  --role repair-worker --session REAL_SESSION
```

Preflight blocks until real native-session and 64K long-prompt evidence exist. `context_tokens=65536` in a profile is a declared limit, not proof of 64K handling. The 4K Qwen3 8B model is not eligible as a 64K Hermes tool agent; it can serve bounded structured tasks when profiled for them.

## Backends and model roles

The registry maps worker, reviewer and security roles explicitly. Loopback Ollama uses the schema-constrained `/api/chat` adapter; llama.cpp uses authenticated OpenAI-style `/v1/chat/completions` with normal certificate checks, no redirects or proxies and no cloud fallback. The adapter checks model identity from `/v1/models` and from the completion. `/health` and `/v1/models` say nothing about long prompts or Hermes terminal use. Hermes `custom` model aliases do not carry into harness subprocesses. Store only a `key_env` reference.

With `repair-dispatch --provider-registry FILE --gpu-policy FILE`, both roles need distinct models, and the worker and reviewer requests run under one host GPU lease. The lease stops a concurrent harness switch but not unrelated systemd or llama.cpp administration; coordinate that separately. Without a registry, repair dispatch uses the loopback Ollama models.

## GPU residency and switching

- Use observed memory figures for your own models (example values: an 8B model about 5 GiB, a 2B fallback about 2 GiB). Keep a reserve and check total capacity before approving a policy; do not assume another model fits while both are resident.
- Mark the 64K fallback `pinned=true` and production or external units `managed=false` until an operator delegates management. Real cross-process serialization needs every request and switch to use the same lock file on `<gpu-host>`; a lock on `<harness-host>` is not distributed.
- Run `gpu-switch` only on `<gpu-host>`, with an approved, narrowly scoped unit policy and `--authorize-switch`. Normal inference never calls it. A switch claims an append-only transaction before stopping anything, confirms the target model's identity and chat, and leaves `UNKNOWN` receipts on errors. Never rerun or force recovery blindly.
- Switching only stops and starts units; it never writes proxy, gateway, DNS or authentication configuration. Pinned and unmanaged units are never evicted. This does not qualify live switching while the production gateway shares the GPU.

## Model evaluations

Collect repeated real Hermes tool traces with a fresh session and an allowed terminal command per attempt. Only terminal tool calls with JSON `{"command":"..."}` are parsed. `model-evaluate` reads offline fixtures (see [`examples-model-eval.json`](examples-model-eval.json)) and records a report only with `--authorize-record`; it never runs commands. Tool-free replies, fabricated or missing terminal receipts, policy denials, wrong function names and execution errors are counted separately. A matching external receipt is still marked `reported_tool_success_requires_origin_review` until Hermes-origin authentication is qualified, so tool-worker admission stays off. Qwen3.5 2B stays analysis-only until head-to-head trials meet the policy thresholds under identical conditions.

```bash
hermes-skill-bridge model-evaluate \
  --sample-file /home/hermes/projects/eval-samples.json \
  --evidence-root /home/hermes/.hermes/harness-live-evidence \
  --session REAL_SESSION
```

## Rollback and infrastructure left alone

- No installer enables harness systemd services on the gateway host. Roll back by stopping only newly approved harness services, pointing the Hermes skill source back at the previous tested checkout and keeping every append-only receipt.
- Leave the reverse proxy TLS, DNS and host mappings unchanged, and check each separately afterwards. Never rewrite the gateway provider chain to test the harness.
- Keep the preflight JSON, the Hermes session transcript, unit states, provider probe results, real 64K prompt and token evidence, GPU contention receipts, reviewed-worktree and functional receipts, and every failure or `UNKNOWN` receipt. Without these host observations, do not claim the deployment is qualified.
