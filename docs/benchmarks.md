# Benchmarks: Rust port vs original Python

Offline wall-time and peak-RSS comparison of each Rust binary against the vendored Python
original (`tests/fixtures/pycorpus/src/hermes_harness`). Nothing touches the network,
Ollama, a gateway or `$HOME`; every stateful pair uses a disposable temp dir and no
`--authorize-*` flag is passed.

## Run

Needs `hyperfine`, `python3`, `git`, cargo, and optionally `/usr/bin/time` (pacman package `time`) for RSS.

```fish
bash bench/run.sh
python3 -I bench/summarize.py bench/results
cat bench/results/SUMMARY.md
```

Variables: `BENCH_RUNS` (default 20), `BENCH_OUT` (default `bench/results`, gitignored),
`CARGO_TARGET_DIR` (default `target`; the binaries are read from `$CARGO_TARGET_DIR/release`).
Fish: `env BENCH_RUNS=50 CARGO_TARGET_DIR=$HOME/hh-target bash bench/run.sh`.

## Method

- `cargo build --release --locked`, then `hyperfine -N -i --warmup 3 --runs N`, Rust and Python side by side per pair.
- Python: `python3 -s -m <module>` with `PYTHONPATH` set to the pycorpus `src`; bytecode cache redirected to a temp dir (`PYTHONPYCACHEPREFIX`), so it is warm after the warmup runs.
- Before timing, each side runs once and both exit codes must match; otherwise the pair is skipped and listed in `skipped.txt`.
- Stateful pairs (`harness-new-ticket`, `supervised-demo`, `agents-assemble`) reset a temp dir (and, for assemble, a fresh one-commit Git repo) in `--prepare`, outside the timed region.
- Peak RSS is one extra run per side under `/usr/bin/time -f %M`.
- Pairs: `--help` for four bins, `hermes-skill-bridge doctor`, `hermes-native game-plan` on `docs/multiagent-example.json`, the gateway contract and lookup probes on a small fixture tree, `hermes-harness new`, the offline supervised demo, and `hermes-native agents-assemble --confirm-offline`.
- Not paired: the other bins (live operator, socket qualification, install) need a host, a socket or write to real locations.

## Caveats

- WSL2, not bare metal. `run.sh` copies the Python sources into the Linux temp dir first, so both sides read from the same filesystem even when the checkout sits on a slow mount such as `/mnt/c`. Bare-metal Arch numbers should be close; the ratios, not the absolute times, are the point.
- The Rust numbers for the sub-millisecond pairs are near process-spawn resolution; the speedup ratios there mostly measure Python interpreter and import startup, not the harness logic.
- `supervised-demo` and `agents-assemble` are dominated by `git` subprocesses on both sides, so their ratios (about 1.1x) show that real work is Git-bound; the port wins there on memory (about 5x less RSS), not wall time.
- Standard deviations for Python are large (WSL2 scheduling); treat ratios as order of magnitude.

## Results (WSL2 Arch run)

- Date: 2026-10-31
- CPU: a 20-thread laptop CPU under WSL2 (Arch)
- rustc 1.99.0, Python 3.14.7, hyperfine 2.0.0; 3 warmup + 20 runs per command

| Pair | Rust mean ± σ (ms) | Python mean ± σ (ms) | Speedup (Py/Rs) | RSS Rust / Python (MiB) |
|---|---|---|---|---|
| agents-assemble | 349.40 ± 44.89 | 386.89 ± 35.07 | 1.1x | 5.2 / 24.7 |
| gateway-contract-probe | 0.51 ± 0.06 | 71.28 ± 24.91 | 138.8x | 2.3 / 15.3 |
| gateway-lookup-probe | 0.58 ± 0.03 | 72.18 ± 23.90 | 124.7x | 2.6 / 15.6 |
| harness-new-ticket | 6.62 ± 1.54 | 169.17 ± 51.76 | 25.6x | 3.1 / 24.1 |
| help-hermes-harness | 0.60 ± 0.03 | 88.42 ± 9.85 | 147.2x | 3.1 / 24.1 |
| help-hermes-native | 0.51 ± 0.11 | 93.29 ± 7.80 | 184.4x | 2.7 / 24.4 |
| help-skill-bridge | 0.76 ± 0.09 | 121.00 ± 9.26 | 159.5x | 3.6 / 25.1 |
| help-supervised-demo | 0.62 ± 0.07 | 124.69 ± 7.09 | 202.5x | 3.3 / 25.4 |
| native-game-plan | 3.34 ± 0.39 | 109.76 ± 41.08 | 32.8x | 2.9 / 24.3 |
| skill-bridge-doctor | 0.71 ± 0.07 | 100.88 ± 4.06 | 143.0x | 3.9 / 25.2 |
| supervised-demo | 381.81 ± 16.26 | 431.48 ± 49.70 | 1.1x | 5.7 / 31.3 |
