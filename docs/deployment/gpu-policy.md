# GPU residency: admission, switching and recovery

Policy, lease and one-shot switch code is tested offline; a real GPU switch or rollback on `<gpu-host>` is not qualified.

## Example inventory

Example values (replace with your own observations): `<gpu-host>` has one 8 GiB GPU and two llama.cpp services:

| Service | Model | Configured context | Approximate VRAM |
|---|---|---:|---:|
| `llama-server-long.service` | Qwen3.5 2B Q4_K_M | 65,536, q8 KV | ~2,000 MiB |
| `llama-server-short.service` | Qwen3 8B Q4_K_M | 4,096 | ~5,200 MiB |
| Combined | | | ~7,200 MiB |

Estimates from deployment, not a live free-memory reading; reserve, KV cache and concurrent requests all matter. A third model of 5 GiB or more cannot be assumed to fit. Default hybrid policy: keep the 64K Qwen3.5 fallback resident and switch only an eligible larger-worker slot with explicit host approval. Fully on-demand residency is opt-in and not qualified.

## Service ownership

- **Pinned:** `pinned=true` protects the fallback from eviction. `managed=false` means an operator, not the harness, owns its lifecycle.
- **Externally managed:** every production llama.cpp unit starts as `managed=false`. Listing a unit in JSON does not authorize stopping it.
- **Harness-managed candidate:** one specific unpinned unit may become `managed=true` only after an explicit per-unit host policy, a safe drain mechanism and operator authorization.
- **Single host:** `gpu_lane` takes a nonblocking `flock` on `lock_path`, which coordinates only processes sharing that filesystem. A lock on `<harness-host>` does not serialize `<gpu-host>`, and other administrators or systemd calls do not take it.

[`docs/examples-gpu-policy.json`](../examples-gpu-policy.json) is not deployable as is: check the real host name, units, observed VRAM, lock directory owner and mode, and free memory first. The registry unit name must match the host service (for example `llama-server-short.service`, not a guessed `llama-short.service`).

## What the implementation does

In `src/rs/deployment/gpu.rs`, `GPUPolicy::admit` sums observed configured units, pinned units and the reserve, and refuses estimated overcommit. `gpu_lane` holds one local exclusive lease. `gpu-switch` checks the process host name against `policy.host`; `switch_gpu_slot` requires `--authorize-switch`, a unique `--transaction` and a durable one-shot claim, refuses protected or unmanaged services, checks start and stop status, observed free VRAM and authenticated target readiness, and records `READY` on success or `UNKNOWN` when uncertain.

There is no automatic rollback from `UNKNOWN`. `UserSystemd` calls `systemctl` without sudo and relies on OS authorization set up beforehand; do not add generic sudo rights to make a demo pass.

## Operator procedure (not a live switching instruction)

1. On `<gpu-host>`, read only: unit status, `nvidia-smi` and the service configs. Record the baseline and any active requests. Never stop the fallback to make room for benchmarks.
2. On the GPU host that owns the lock, validate a reviewed policy and existing private lock and journal directories (no symlinks, exact unit names). Dry-run admission and the offline contention tests before considering a switch.
3. On `<harness-host>`, run only read-only provider, preflight and model-quality checks. Never run `gpu-switch` there.
4. A real switch needs a fresh scoped operator grant, explicit drain evidence, one unique transaction ID and proven permissions on `<gpu-host>`; an independent owner confirms no protected requests are live.
5. On `UNKNOWN` or interruption, stop retrying, keep `TX-claim.json` and `TX-result.json`, inspect service status, VRAM, active requests and target identity, then follow a separately approved manual rollback. Never delete a lock or receipt to restart the same transaction.

### Read-only commands (on `<gpu-host>`)

```bash
systemctl is-active llama-server-short.service llama-server-long.service
systemctl show llama-server-short.service llama-server-long.service -p User -p ActiveState
nvidia-smi --query-gpu=memory.used,memory.free,memory.total --format=csv,noheader
```

These do not show that a service can be switched safely. Do not copy API keys to this host for a GPU test, and never edit the proxy, DNS or gateway credentials for GPU switching.

See [gateway host install](gateway-host.md), [provider routing](provider-routing.md) and [acceptance tests](acceptance-tests.md).
