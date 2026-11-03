# Gateway host install, upgrade and rollback

Checked offline against the source; installation and gateway-originated skill invocation on a live `<harness-host>` are not qualified. This is an operator runbook, not deployment authorization: leave the running Hermes gateway, its provider credentials, messaging settings and model aliases alone unless separately approved. Read [Hermes workflows](../user-guide/hermes-workflows.md) and [acceptance tests](acceptance-tests.md) first.

## Inventory and boundaries

| Host | Role |
|---|---|
| `<harness-host>` | Runs the gateway as `hermes` (system user, nologin, private `/home/hermes` with mode 0700) via `hermes-gateway-<id>.service` |
| `<gpu-host>` | Single GPU; llama.cpp units for a short-context and a long-context model (example values: 8 GiB GPU, 4K-context 8B model on :8080, 64K-context 2B model on :8081). Do not install a GPU switcher on `<harness-host>` |
| Reverse proxy | Serves `llm.example.home.arpa` and `llm-long.example.home.arpa` over HTTPS with a backend bearer token |
| `<dns-host>` | Resolver for `<harness-host>`; forwards `home.arpa` internally |

This runbook never changes the proxy, firewall, DNS, `/home/hermes/.hermes/.env` or the gateway unit.

## Step 0: read-only baseline

On `<harness-host>`, from an operator shell:

```bash
systemctl show hermes-gateway-<id>.service -p User -p WorkingDirectory -p ActiveState
getent hosts llm.example.home.arpa llm-long.example.home.arpa
test -x /home/hermes/.local/bin/uv && echo "hermes uv present"
sudo -u hermes -H /bin/bash -c 'cd /home/hermes && id && pwd && /home/hermes/.local/bin/hermes --version'
```

Expect `User=hermes` and `WorkingDirectory=/home/hermes/.hermes`. If anything disagrees, stop. Do not change permissions on other users' home directories to make the service user work.

## Step 1: stage a pinned, reviewed checkout and private state

Record the exact reviewed 40-character commit on the workstation; never pin a floating branch. Give `hermes` an approved read-only Git credential or transfer a verified archive; do not copy the operator's tokens or SSH keys into the service account. After operator approval:

```bash
sudo install -d -o hermes -g hermes -m 0700 /home/hermes/projects /home/hermes/projects/worktrees \
  /home/hermes/.config/hermes-harness /home/hermes/.local/state/hermes-harness/evidence
sudo -u hermes -H /bin/bash -c 'cd /home/hermes/projects && git clone --no-checkout <harness-repo-url> hermes-harness-qualification &&
  cd hermes-harness-qualification && git checkout --detach REVIEWED_SHA && git rev-parse HEAD && cargo build --release --locked'
```

Never update the gateway source tree or an existing harness checkout in place, and never overwrite earlier receipts or worktrees. `deploy-preflight` checks for Python >=3.14 (used by `functional-verify` and `repair-dispatch`) and `uv` on the service `PATH`; if either or Git access is missing, record `BLOCK` and fix it separately.

Edit the illustrative `docs/examples-provider-registry.json` and `docs/examples-gpu-policy.json` before copying them with `install -o hermes -g hermes -m 0600`. JSON holds only `key_env` (for example `LOCAL_LLM_API_KEY`), never a key; the key must be in the environment of the process that runs the probe, which may differ from the gateway's and your shell's. Put the proxy CA in the system trust store; never use `curl -k` or disable TLS checks.

## Step 2: read-only preflight, before skill registration

Use a clearly labeled diagnostic session. Omit `--gpu-policy` on the first pass: the GPU lease check creates a local lock file if its parent exists, and a lock on `<harness-host>` does not protect `<gpu-host>`.

```bash
sudo -u hermes -H /bin/bash -c 'cd /home/hermes && projects/hermes-harness-qualification/target/release/hermes-skill-bridge deploy-preflight \
  --workspace /home/hermes/projects/worktrees --gateway-unit hermes-gateway-<id>.service --session operator-diagnostic-unqualified'
```

Expected: JSON `checks` with `PASS`, `WARN` or `BLOCK`; `native_session`, `context_64k` and `tool_qualification` stay `BLOCK`, and so does `provider` without a registry. `ready=false` does not mean the gateway failed. `--probe-network` qualifies only short authenticated requests, not 64K tool use. With a separate network-inference grant, pass a reviewed registry and run `provider-probe` per role; see [provider routing](provider-routing.md).

## Step 3: stage skills, then approve live discovery

`hermes-skill-install` stages five instruction-only skills: `lets-investigate`, `game-plan`, `agents-assemble`, `what-we-know` and `cyber-warrior`. They are not registered slash-command APIs. Compare `hermes_skills/*/SKILL.md` with `/home/hermes/.hermes/skills` first, then, after an install approval, copy only reviewed skill folders as `hermes`. For one new skill:

```bash
sudo install -d -o hermes -g hermes -m 0700 /home/hermes/.hermes/skills
sudo -u hermes -H cp -R /home/hermes/projects/hermes-harness-qualification/hermes_skills/lets-investigate /home/hermes/.hermes/skills/
```

Never copy over an existing skill without comparing and backing it up. List skills as `hermes` (`hermes skills list` or the installed version's equivalent), then invoke `/lets-investigate` from an isolated Hermes session and capture the model and tool transcript and the returned Git HEAD. Until that works, skill invocation and session binding stay unqualified. Check the installed version before assuming a reload or gateway restart is needed.

## Upgrades and rollback

- Upgrade by building a second pinned hermes-owned checkout, running its preflight first, and switching the reviewed skill source only after owner approval. Do not change the Hermes install or restart the gateway to test the harness.
- Roll back by no longer invoking the new checkout and restoring only the skill definitions this trial replaced. Keep all evidence, dispatch fences, worktrees and `UNKNOWN` receipts; never repeat non-idempotent inference.
- Check the gateway separately with `systemctl is-active hermes-gateway-<id>.service`. If the proxy, DNS or `<gpu-host>` looks broken, follow that service's own runbook instead of restarting the gateway.
