# Hermes Harness

Ticket-driven multi-agent orchestration for [Hermes](https://github.com/NousResearch/hermes-agent), written in Rust and shipped as static binaries. It is a port of Seth Barrett's Python `hermes-harness`.

Hermes is an agent you chat with through its gateway; it loads skills (`SKILL.md` instruction files) and runs commands through its terminal tool. The harness is for operators who want Hermes to change code under supervision. Its skills in `hermes_skills/` call the harness binaries; the offline demos below need no Hermes at all.

A coordinator model plans, and cheaper local worker and reviewer models propose changes. Every step that changes a repository or runs code sits behind an evidence digest and an explicit operator decision: inference does not authorize application, application does not authorize execution, and a passing test does not authorize merging.

- Investigate, plan, review, apply and verify each produce their own digest and decision point.
- Default paths run deterministic doubles. Nothing touches a network, gateway or live repository unless you ask, and each live action needs its own `--authorize-*` flag.
- One crate, twelve binaries, no Python environment for the harness itself.

## Build

Requires Rust 1.88 or newer (edition 2024), Git, make, and Python 3 for the tests. `make check` also needs the rustfmt and clippy components (`rustup component add rustfmt clippy`). Linux is the target: the sandbox and broker refuse non-Unix hosts by design. Windows builds but skips the Unix-only tests, and the Makefile is untested there.

```sh
git clone https://github.com/SamuelSchwertfeger/hermes-harness-rs && cd hermes-harness-rs
cargo build --release --locked
make check        # rustfmt, clippy, tests
```

On Arch, CachyOS or Debian 13 you can build a package instead; see [Install as a package](#install-as-a-package).

## Quick start without Hermes

From a checkout build; after a package install, drop the `target/release/` prefix.

```sh
target/release/hermes-skill-bridge doctor                        # read-only health check
target/release/hermes-supervised-demo                            # offline demo in a throwaway Git repo
target/release/hermes-native agents-assemble --confirm-offline   # deterministic doubles, no network
```

## Use with Hermes

You need a Linux host running Hermes, with an existing skills root such as `~/.hermes/skills`. Stage the five supported skills into a new `hermes-harness` category; run without `--apply` first for a dry run. An existing category is never replaced.

```sh
target/release/hermes-skill-install --source hermes_skills --target ~/.hermes/skills --apply   # from a checkout
hermes-skill-install --source /usr/share/hermes-harness/skills --target ~/.hermes/skills --apply # after a package install
```

The staged copies under `~/.hermes/skills/hermes-harness/` still call `/ABS/PATH/TO/HARNESS/target/release/hermes-native` and `.../hermes-skill-bridge`. In each one, replace the whole `/ABS/PATH/TO/HARNESS/target/release` prefix with your checkout's absolute `target/release` directory, or with `/usr/bin`. Confirm Hermes lists them (`hermes skills list` or your version's equivalent), then in a Hermes session:

> `/lets-investigate Inspect the Git metadata for /path/to/repo. Do not modify or run repository code.`

Skill discovery and session binding on a live gateway are not yet qualified. The Rust build also lacks the production gateway connector: `hermes-probe` gateway operations exit 2 with `Hermes gateway client unavailable in Rust build; use the Python probe`. Skills are unaffected, since they run the binaries through Hermes' terminal tool. Details: [Hermes workflows](docs/user-guide/hermes-workflows.md), [gateway host install](docs/deployment/gateway-host.md).

## Binaries

| Binary | Purpose |
|---|---|
| `hermes-harness` | Ticket state and routing (`new`, `show`, `advance`, `approve`, `evidence`); offline `multiagent` DAG runs |
| `hermes-native` | `lets-investigate`, `game-plan`, `agents-assemble`, `inspect` |
| `hermes-skill-bridge` | Main operator CLI: `doctor`, `plan`, `live-review`, `apply-reviewed`, `verify-applied`, `functional-verify`, `sandbox-tool`, `provider-probe` and others |
| `hermes-skill-install` | Stages skill manifests (dry run unless `--apply` is given) |
| `hermes-probe` | Operator probes (admission, recovery, `ollama-smoke`) |
| `hermes-supervised-demo`, `hermes-supervised-status` | Offline demo; read-only view of retained evidence |
| `hermes-gateway-*`, `hermes-live-*` | Gateway probes and read-only host inspection |

`--help` lists every subcommand. `HERMES_LOG=error|warn|info|debug|trace` writes one JSON object per line to stderr. Logging is off by default, and arguments are never logged.

## Install as a package

Give the harness its own Arch, CachyOS or Debian host or VM. Both packages install `/usr/bin/hermes-*`, the skill manifests under `/usr/share/hermes-harness/skills/`, and a hardened systemd user timer that runs `hermes-skill-bridge doctor` daily. Runtime dependencies are `git` and `util-linux`; add `bubblewrap` for `functional-verify` and `sandbox-tool`.

### Arch and CachyOS

`makepkg` builds from a committed checkout, runs the tests and packages the result. It needs `cargo`, `git` and `python` (the tests call Python 3), nothing from the AUR. Fish instructions: [docs/deployment/cachyos.md](docs/deployment/cachyos.md).

```sh
cd packaging/arch && makepkg -f
sudo pacman -U hermes-harness-*.pkg.tar.zst
```

### Debian 13

Debian 13 ships `rustc` 1.85, so install the toolchain with rustup:

```sh
sudo apt install build-essential git curl python3     # python3 is used by the tests
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --profile minimal
. "$HOME/.cargo/env"
```

`packaging/debian/build-deb.sh` clones the committed checkout, builds, runs the tests and writes a `.deb` into the current directory. `apt` installs it with the recommended `bubblewrap` and `python3`.

```sh
bash packaging/debian/build-deb.sh
sudo apt install ./hermes-harness_*.deb
```

[docs/deployment/debian.md](docs/deployment/debian.md) covers `cargo install`, skipping tests with `DEB_BUILD_OPTIONS=nocheck`, and uninstalling.

### Health timer

```sh
systemctl --user daemon-reload
systemctl --user enable --now hermes-harness-health.timer
loginctl enable-linger $USER     # keep the timer firing while logged out
journalctl --user -u hermes-harness-health -o cat | jq
```

Status 226/NAMESPACE means unprivileged user namespaces are off. Set `kernel.unprivileged_userns_clone` to 1, or drop the sandboxing lines with `systemctl --user edit hermes-harness-health.service`.

### Provider registry

Local inference is declared in a JSON registry with a `profiles` array and a `roles` map ([example](docs/examples-provider-registry.json), [rules](docs/deployment/provider-routing.md)). Two backends are allowed:

| Backend | Rule |
|---|---|
| `ollama` | Plain `http://` on a loopback address only; no `key_env` |
| `llama.cpp` | `https://`, hostname under `.home.arpa`, bearer key named by `key_env` |

An Ollama box elsewhere on the LAN is declared as a `llama.cpp` profile at `https://<ollama-host>.home.arpa/v1/chat/completions`, behind an internal TLS proxy whose CA the OS trusts. Key values live only in the process environment, never in JSON, Git or argv.

```sh
hermes-skill-bridge provider-probe --provider-registry <path> --role <role> --session <session>
```

Back up `~/.config/hermes-harness` and `~/.local/state/hermes-harness` (mode 0700; receipts, ledgers and evidence, never key values). To upgrade, build a new package from a reviewed, pinned commit.

## Compared with the Python original

The Python sources are vendored under `tests/fixtures/pycorpus/` (snapshot of 2026-10-30), and fixtures generated by CPython 3.14 pin the JSON output and digests. A CI differential suite against CPython 3.14 found several of the divergences in [DIVERGENCES.md](DIVERGENCES.md).

### Gains

- **Deployment:** static binaries instead of a Python environment. The host still runs Hermes, and `functional-verify` and `repair-dispatch` still call `/usr/bin/python3`.
- **Startup:** help and probe commands take about 0.5 to 0.8 ms against 70 to 125 ms. Creating a ticket takes 6.6 ms against 169 ms.
- **Memory:** peak RSS is 2 to 6 MiB against 15 to 31 MiB.
- **Packaging and logs:** an Arch `PKGBUILD`, a Debian package script, a hardened systemd health timer, and opt-in JSON logs.

### Costs and changes

- **Git-bound work gains little.** `supervised-demo` and `agents-assemble` are only about 1.1x faster because `git` dominates; memory is about 5x lower. The large ratios on short commands mostly measure Python startup.
- **Benchmarks are limited.** They come from one WSL2 run, so read the ratios as orders of magnitude ([method](docs/benchmarks.md)).
- **Stricter JSON decoding.** The broker and Ollama decoders reject UTF-16/32, a UTF-8 BOM, lone surrogates, `NaN`/`Infinity`, nesting of 128 or deeper, and integers above `u64::MAX`. Python accepts some of these.
- **Other divergences.** Non-UTF-8 paths are refused, help text differs from argparse, probe output is compact JSON, `ollama-smoke` cannot syntax-check Python, and the gateway connector is missing ([above](#use-with-hermes)).
- **Maintenance.** Contributors need Rust, and the port tracks a frozen upstream snapshot.

## Development

`make check` runs rustfmt, clippy and tests; `make ci` adds `cargo deny` and `cargo machete`, matching CI. CI also runs an Arch container job, a Debian package build, an MSRV check, the differential suite, a 30 s fuzz smoke per target, and coverage.

- **rustfmt** with the settings in `rustfmt.toml`.
- **clippy** with the `all`, `pedantic` and `nursery` groups set to deny (`[lints]` in `Cargo.toml`), run as `cargo clippy --all-targets -- -D warnings`. `unwrap`, `expect`, `panic`, `todo`, `dbg!` and unchecked indexing are denied too; tests may use them. The complexity limits in `clippy.toml` mirror the ruff rules of the Python original.
- **rustc:** `unsafe_code` is forbidden. `missing_docs`, `unreachable_pub` and `unused_qualifications` are warnings, which `-D warnings` turns into errors in CI.
- **cargo deny** (licenses, advisories, sources; `deny.toml`) and **cargo machete** (unused dependencies).
- **Fuzzing:** `cargo +nightly fuzz run broker_request` (needs nightly Rust and cargo-fuzz; targets are in `fuzz/fuzz_targets/`).
## Documentation

- [docs/user-guide/](docs/user-guide/): workflows, approval and recovery
- [docs/deployment/](docs/deployment/): live-gateway deploy, CachyOS, Debian 13, acceptance tests, GPU policy, sandbox tools, provider routing
- [docs/rust-port/](docs/rust-port/): layout conventions, removed-script map, test gaps
- [docs/multiagent-runtime.md](docs/multiagent-runtime.md), [docs/benchmarks.md](docs/benchmarks.md), [PROJECT_STATUS.md](PROJECT_STATUS.md) for known limits

## License and credit

MIT. Copyright (c) 2026 Seth Barrett and Samuel Schwertfeger. A port of Seth Barrett's Python `hermes-harness`, made with his permission. See [LICENSE](LICENSE).
