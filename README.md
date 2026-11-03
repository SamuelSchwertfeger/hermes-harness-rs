# Hermes Harness

Ticket-driven multi-agent orchestration for Hermes, written in Rust and shipped as static binaries. It is a port of Seth Barrett's Python hermes-harness (private upstream).

A coordinator model plans, and cheaper local worker and reviewer models propose changes. Every step that changes a repository or runs code sits behind an evidence digest and an explicit operator decision. Inference does not authorize application, application does not authorize execution, and a passing test does not authorize merging.

## Why use it

- Investigate, plan, review, apply and verify each get their own digest and decision point, so every step can be audited.
- Default paths run deterministic doubles. Nothing touches a network, gateway or live repository unless you ask.
- Each live action needs its own `--authorize-*` flag.
- One crate, twelve binaries, no Python venv for the harness itself.

## Install and build

Requires Rust 1.88 or newer (edition 2024).

```sh
git clone https://github.com/SamuelSchwertfeger/hermes-harness-rs && cd hermes-harness-rs
cargo build --release --locked
make check        # fmt, clippy, tests (make ci adds cargo deny and cargo machete)
```

On Arch, CachyOS or Debian 13 you can build a package instead, see [Homelab setup](#homelab-setup).

## Quick start

Day to day you talk to Hermes, and the skills in `hermes_skills/` call `hermes-native` and `hermes-skill-bridge`. To try it without a gateway:

```sh
target/release/hermes-skill-bridge doctor                        # read-only health check
target/release/hermes-supervised-demo                            # offline demo in a throwaway Git repo
target/release/hermes-native agents-assemble --confirm-offline   # deterministic doubles, no network
```

A skill being in Git does not mean it is installed on a live gateway. See [docs/user-guide/hermes-workflows.md](docs/user-guide/hermes-workflows.md).

## Binaries

| Binary | Purpose |
|---|---|
| `hermes-harness` | Ticket state and routing (`new`, `show`, `advance`, `approve`, `evidence`); offline `multiagent` DAG runs |
| `hermes-native` | `lets-investigate`, `game-plan`, `agents-assemble`, `inspect` |
| `hermes-skill-bridge` | Main operator CLI: `doctor`, `plan`, `live-review`, `apply-reviewed`, `verify-applied`, `functional-verify`, `sandbox-tool`, `provider-probe`, and more |
| `hermes-skill-install` | Stages skill manifests (dry run without `--apply`) |
| `hermes-probe` | Operator probes (admission, recovery, `ollama-smoke`) |
| `hermes-supervised-demo`, `hermes-supervised-status` | Offline demo; read-only view of retained evidence |
| `hermes-gateway-*`, `hermes-live-*` | Gateway probes and read-only host inspection |

`--help` lists every subcommand. Set `HERMES_LOG=error|warn|info|debug|trace` for one JSON log object per line on stderr. It is off by default, and arguments are never logged.

## Homelab setup

Give the harness its own Arch, CachyOS or Debian host or VM (fish instructions: [docs/deployment/cachyos.md](docs/deployment/cachyos.md)). `makepkg` builds from a committed checkout, runs the tests and packages it. It needs `cargo` and `git`, no AUR.

```sh
cd packaging/arch && makepkg -f
sudo pacman -U hermes-harness-*.pkg.tar.zst
```

Runtime dependencies are `git` and `util-linux`; add `bubblewrap` for `functional-verify` and `sandbox-tool`. The package installs `/usr/bin/hermes-*`, the skill manifests, and a hardened systemd user timer that runs `hermes-skill-bridge doctor` daily:

```sh
systemctl --user daemon-reload
systemctl --user enable --now hermes-harness-health.timer
loginctl enable-linger $USER     # keep the timer firing while logged out
journalctl --user -u hermes-harness-health -o cat | jq
```

Status 226/NAMESPACE means unprivileged user namespaces are off. Set `kernel.unprivileged_userns_clone` to 1, or drop the sandboxing lines with `systemctl --user edit hermes-harness-health.service`.

### Debian 13

The crate needs Rust 1.88 or newer and Debian 13 ships `rustc` 1.85, so install the toolchain with rustup:

```sh
sudo apt install build-essential git curl python3     # python3 is used by the tests
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --profile minimal
. "$HOME/.cargo/env"
```

`packaging/debian/build-deb.sh` does what `makepkg` does above: it clones the committed checkout, builds, runs the tests and writes a `.deb` into the current directory. apt installs it together with the recommended `bubblewrap` and `python3`.

```sh
bash packaging/debian/build-deb.sh
sudo apt install ./hermes-harness_*.deb
```

The package installs the same files and the same health timer as the Arch package, so enable it with the `systemctl --user` steps above. [docs/deployment/debian.md](docs/deployment/debian.md) covers the `cargo install` alternative, `DEB_BUILD_OPTIONS=nocheck` to skip the tests, and uninstalling.

### Provider registry

Local inference is declared in a JSON registry (`profiles` plus a `roles` map; [example](docs/examples-provider-registry.json), [rules](docs/deployment/provider-routing.md)). Two shapes are allowed:

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

Both versions aim at the same behavior. The Python sources are vendored under `tests/fixtures/pycorpus/`, and fixtures generated by CPython 3.14 pin the JSON output and digests. Sources: [DIVERGENCES.md](DIVERGENCES.md), [docs/benchmarks.md](docs/benchmarks.md), the CI config and `packaging/`.

### What the port improves

- **Deployment:** static binaries instead of a Python venv for the harness. The host still runs Hermes, and `functional-verify` and `repair-dispatch` still call `/usr/bin/python3`.
- **Startup:** help and probe commands take about 0.5 to 0.8 ms against 70 to 125 ms. Creating a ticket takes 6.6 ms against 169 ms.
- **Memory:** peak RSS is 2 to 6 MiB against 15 to 31 MiB.
- **Testing:** CI runs a differential suite against CPython 3.14, an Arch container job, an MSRV check, a 30 s fuzz smoke, `cargo deny`, coverage, and clippy with `-D warnings`. `unsafe` is forbidden and `unwrap`, `expect` and `panic` are denied by lint. The differential suite found several of the divergences below.
- **Packaging and logs:** an Arch `PKGBUILD`, a hardened systemd health timer, and opt-in JSON logs.

### What it gives up or changes

- **Git-bound work gains little.** `supervised-demo` and `agents-assemble` are only about 1.1x faster because `git` dominates; memory is about 5x lower. The large ratios on short commands mostly measure Python startup.
- **Benchmarks are limited.** They come from one WSL2 run, so read the ratios as orders of magnitude ([method](docs/benchmarks.md)).
- **JSON decoding is stricter.** The broker and Ollama decoders reject UTF-16/32, a UTF-8 BOM, lone surrogates, `NaN`/`Infinity`, nesting of 128 or deeper, and integers above `u64::MAX`. Python accepts some of these.
- **Documented divergences.** Non-UTF-8 paths are refused, help text differs from argparse, probe output is compact JSON, and `ollama-smoke` cannot syntax-check Python. Full list in [DIVERGENCES.md](DIVERGENCES.md).
- **Gateway probes are not ported.** The production gateway connector exits 2 with `Hermes gateway client unavailable in Rust build; use the Python probe`.
- **Linux comes first.** The sandbox and broker refuse non-Unix hosts by design. Windows builds but runs fewer tests (649 passed against 710 on Arch), and the Makefile is untested there.
- **Maintenance.** Contributors need Rust, and the port tracks a frozen upstream snapshot (2026-10-30, vendored under `tests/fixtures/pycorpus/`).

## Linting

Standard Rust tooling, set stricter than most crates. There is no Google or other house style. `make check` runs rustfmt, clippy and the tests; `make ci` adds cargo deny and cargo machete, matching CI.

- **rustfmt** with the settings in `rustfmt.toml`.
- **clippy** with the `all`, `pedantic` and `nursery` groups set to deny (`Cargo.toml` `[lints]`), run as `cargo clippy --all-targets -- -D warnings`. `unwrap`, `expect`, `panic`, `todo`, `dbg!` and unchecked indexing are denied as well. These choices, and the complexity limits in `clippy.toml`, mirror the ruff rules of the Python original. Tests may use `unwrap`, `expect`, `panic` and indexing.
- **rustc:** `unsafe_code` is forbidden. `missing_docs`, `unreachable_pub` and `unused_qualifications` are warnings, which `-D warnings` turns into errors in CI.
- **cargo deny** (licenses, advisories, sources; `deny.toml`) and **cargo machete** (unused dependencies).

If a Rust update adds `nursery` lints that break the build, downgrade that group to `warn` instead of scattering `allow`s.

## Documentation

- [docs/user-guide/](docs/user-guide/): workflows, approval and recovery
- [docs/deployment/](docs/deployment/): live-gateway deploy, Debian 13, acceptance tests, GPU policy, sandbox tools, provider routing
- [docs/rust-port/](docs/rust-port/): layout conventions, removed-script map
- [docs/multiagent-runtime.md](docs/multiagent-runtime.md), [docs/benchmarks.md](docs/benchmarks.md), [PROJECT_STATUS.md](PROJECT_STATUS.md) for known limits
- Fuzzing: `cargo +nightly fuzz run broker_request` (needs nightly Rust and cargo-fuzz)

## License and credit

MIT. Copyright (c) 2026 Seth Barrett and Samuel Schwertfeger. A port of Seth Barrett's Python `hermes-harness`, made with his permission. See [LICENSE](LICENSE).
