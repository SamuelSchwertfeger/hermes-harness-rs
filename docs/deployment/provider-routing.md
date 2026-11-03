# Provider routing: coordinator and local workers

Registry, probe and transport code is tested offline; harness inference from `<harness-host>` to `<gpu-host>` is not qualified. Harness routing is independent of the Hermes gateway's own fallback routing.

## Two routing systems

1. **Hermes gateway primary and fallback.** The gateway has its own primary provider and aliases (an alias may point at a long-context endpoint). Automatic failover has not been induced or qualified. Do not rewrite the gateway's credentials or configuration to set up the harness.
2. **Harness model registry.** `src/rs/deployment/providers.rs` reads an operator-written JSON registry, maps each `role` to a profile, and picks loopback Ollama or authenticated OpenAI-compatible llama.cpp transport per dispatch. Hermes aliases do not reach worker subprocesses; neither the model nor the coordinator can rewrite role routing.

### Endpoints (inventory, not model qualification)

| Endpoint | Advertised configuration | Provisional role |
|---|---|---|
| `https://llm.example.home.arpa/v1` to `<gpu-host>:8080` | Qwen3 8B Q4_K_M, 4,096 tokens, `llama-server-short.service` | Short-context bounded structured inference only |
| `https://llm-long.example.home.arpa/v1` to `<gpu-host>:8081` | Qwen3.5 2B Q4_K_M, 65,536 configured, `llama-server-long.service` | Lightweight analysis and fallback; tool execution not qualified |
| `http://127.0.0.1:11434` | Loopback Ollama | Only where a local Ollama service exists |

Backend ports accept only the reverse proxy. Clients reach the proxy over HTTPS with an internal CA; the proxy-to-backend hop is plain HTTP on a restricted LAN. Confirm model IDs with an authenticated `/v1/models` before live use, and never remap a model because the server reports a different ID.

## Registry configuration

Examples: [provider registry](../examples-provider-registry.json) and [GPU policy](../examples-gpu-policy.json). The registry is a strict JSON object with a `profiles` array and a `roles` map. Each profile has `name`, `backend` (`llama.cpp` or `ollama`), `model`, a full `url` ending in `/v1/chat/completions`, `context_tokens`, `key_env`, `tool_calls`, and optional `unit`, `gpu_host` and `max_concurrency` (currently must be 1).

```json
{
  "profiles": [
    {
      "name": "qwen35-long",
      "backend": "llama.cpp",
      "model": "REPLACE_WITH_ACTUAL_V1_MODELS_ID",
      "url": "https://llm-long.example.home.arpa/v1/chat/completions",
      "context_tokens": 65536,
      "key_env": "LOCAL_LLM_API_KEY",
      "tool_calls": false,
      "unit": "llama-server-long.service",
      "gpu_host": "<gpu-host>",
      "max_concurrency": 1
    }
  ],
  "roles": { "analysis-reader": "qwen35-long" }
}
```

The model ID is a placeholder. Never put the value of `LOCAL_LLM_API_KEY` in JSON, Git, shell history, logs or CLI arguments. `key_env` must resolve in the calling service-user process, whose environment may differ from the gateway's.

Validation rejects URL userinfo, query and fragment, non-loopback Ollama, remote non-HTTPS and llama.cpp without bearer auth, and checks the declared model ID against the catalogue and a short completion. It does not test a real 65,536-token prompt, tool-use reliability or the authenticity of a human authorization.

## Read-only diagnostics and inference

**Read-only:** name resolution, proxy TLS negotiation, unit status and an HTTPS `401` without a key. Do not print tokens.

**Opt-in inference:** `provider-probe` sends one short authenticated chat request; the operator authorizes that call separately. Run as `hermes` from `/home/hermes`:

```bash
/home/hermes/projects/hermes-harness-qualification/target/release/hermes-skill-bridge provider-probe \
  --provider-registry /home/hermes/.config/hermes-harness/providers.json \
  --role analysis-reader --session ACTUAL_OR_EXPLICIT_DIAGNOSTIC_SESSION
```

The output records `model_identity_verified` and does not certify long context. A 401 or 403 is an authentication or boundary failure, never a reason to disable TLS. A 4K model must not stand in for a 64K Hermes tool agent; the registry blocks the unqualified `hermes-tool-worker` and `hermes-coordinator` roles even when a profile sets `tool_calls=true`.

## Repair worker and reviewer routing

Only for an existing authorized repair reservation, run `repair-dispatch --provider-registry FILE --gpu-policy FILE` with distinct models for `repair-worker` and `repair-reviewer`. The request is one-shot and leaves immutable evidence; registry-selected local roles never fall back to a cloud model.

The GPU policy must match any remote `gpu_host` names. A lock on `<harness-host>` does not synchronize with `<gpu-host>`, so remote units stay externally managed. A successful `provider-probe` is not permission to switch models or run generated code.

See [GPU policy](gpu-policy.md), [model evaluation](../deployment-qualification.md#model-evaluations) and [acceptance tests](acceptance-tests.md).
