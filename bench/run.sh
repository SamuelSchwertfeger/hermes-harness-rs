#!/usr/bin/env bash
# Rust port vs original Python hermes_harness, offline, with hyperfine.
# Env: BENCH_RUNS (20), BENCH_OUT (bench/results), CARGO_TARGET_DIR (target).
# Every pair is offline and uses disposable temp dirs; no --authorize-* flag is used.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNS="${BENCH_RUNS:-20}"
OUT="${BENCH_OUT:-$ROOT/bench/results}"
TARGET="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET" in /*) ;; *) TARGET="$ROOT/$TARGET" ;; esac
BIN="$TARGET/release"
PYSRC="$ROOT/tests/fixtures/pycorpus/src"

command -v hyperfine >/dev/null || { echo "hyperfine not found" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 not found" >&2; exit 1; }

mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/hh-bench.XXXXXX")"
trap 'find "$TMP" -delete' EXIT

(cd "$ROOT" && cargo build --release --locked)

# Copy the Python sources next to the temp dir so both sides read from the same filesystem
# (a checkout on a slow mount, such as WSL's /mnt/c, would otherwise inflate Python times).
cp -r "$PYSRC" "$TMP/pysrc"
export PYTHONPATH="$TMP/pysrc"
export PYTHONPYCACHEPREFIX="$TMP/pyc"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
PY="python3 -s -m"

HAVE_TIME=0
if [ -x /usr/bin/time ] && /usr/bin/time -f %M true 2>/dev/null; then HAVE_TIME=1; fi
[ "$HAVE_TIME" = 1 ] || echo "note: /usr/bin/time missing, RSS skipped" >&2

# Fixtures (read-only for the probes).
FIX="$TMP/fix"
mkdir -p "$FIX/gateway"
cat >"$FIX/gateway/session_controls.py" <<'PYEOF'
class Gateway:
    async def submit(self, params):
        return {'admission_id': 'abc', 'state': 'queued'}
    def receipt(self, params):
        return {'submission_id': 'def', 'state': 'running'}
    def cancel(self, params):
        return {'status': 'requested'}
PYEOF
cat >"$FIX/lookup.py" <<'PYEOF'
class Admissions:
    def find_by_submission_id(self, submission_id: str):
        return self.db.execute('SELECT * FROM admissions WHERE submission_id = ?', (submission_id,))
PYEOF

D="$TMP/work" # disposable dir, reset before every run
REPO="$D/repo"
PLAN="$ROOT/docs/multiagent-example.json"

# Reset script, also used by hyperfine --prepare. Arg: dir | repo.
cat >"$TMP/prep.sh" <<PREPEOF
#!/usr/bin/env bash
set -e
D='$D'
if [ -d "\$D" ]; then find "\$D" -delete; fi
mkdir -p "\$D"
if [ "\${1:-}" = repo ]; then
  mkdir -p '$REPO'
  git -C '$REPO' init -q
  git -C '$REPO' config user.email bench@example.invalid
  git -C '$REPO' config user.name bench
  echo baseline >'$REPO/README.md'
  git -C '$REPO' add README.md
  git -C '$REPO' commit -q -m baseline
fi
PREPEOF

prep() { if [ -n "$1" ]; then bash "$TMP/prep.sh" "$1"; fi; }

# pair NAME PREPMODE "RUST CMD" "PYTHON CMD"   (PREPMODE: "" | dir | repo)
pair() {
  local name="$1" mode="$2" rcmd="$3" pcmd="$4" rc_r rc_p
  prep "$mode"
  set +e
  $rcmd >/dev/null 2>&1
  rc_r=$?
  set -e
  prep "$mode"
  set +e
  $pcmd >/dev/null 2>&1
  rc_p=$?
  set -e
  if [ "$rc_r" != "$rc_p" ]; then
    echo "SKIP $name: exit codes differ (rust=$rc_r python=$rc_p)" | tee -a "$OUT/skipped.txt"
    return 0
  fi
  echo "== $name (exit $rc_r on both)"
  local prepargs=()
  if [ -n "$mode" ]; then prepargs=(--prepare "bash $TMP/prep.sh $mode"); fi
  hyperfine -N -i --warmup 3 --runs "$RUNS" "${prepargs[@]}" \
    -n rust "$rcmd" -n python "$pcmd" \
    --export-json "$OUT/$name.json" --export-markdown "$OUT/$name.md"
  if [ "$HAVE_TIME" = 1 ]; then
    local f="$TMP/rss" kr kp
    prep "$mode"
    # shellcheck disable=SC2086 # word splitting of the command is intended
    /usr/bin/time -f %M -o "$f" $rcmd >/dev/null 2>&1 || true
    kr="$(tail -n1 "$f")"
    prep "$mode"
    # shellcheck disable=SC2086
    /usr/bin/time -f %M -o "$f" $pcmd >/dev/null 2>&1 || true
    kp="$(tail -n1 "$f")"
    printf 'rust %s\npython %s\n' "$kr" "$kp" >"$OUT/$name.rss"
  fi
}

find "$OUT" -maxdepth 1 -type f \( -name '*.json' -o -name '*.md' -o -name '*.rss' -o -name skipped.txt \) -delete

pair help-hermes-harness "" "$BIN/hermes-harness --help" "$PY hermes_harness.cli --help"
pair help-hermes-native "" "$BIN/hermes-native --help" "$PY hermes_harness.native --help"
pair help-skill-bridge "" "$BIN/hermes-skill-bridge --help" "$PY hermes_harness.native.skill_bridge --help"
pair help-supervised-demo "" "$BIN/hermes-supervised-demo --help" "$PY hermes_harness.agents.supervised_demo --help"
pair skill-bridge-doctor "" "$BIN/hermes-skill-bridge doctor" "$PY hermes_harness.native.skill_bridge doctor"
pair native-game-plan "" "$BIN/hermes-native game-plan --plan $PLAN" \
  "$PY hermes_harness.native game-plan --plan $PLAN"
pair gateway-contract-probe "" "$BIN/hermes-gateway-contract-probe $FIX" \
  "$PY hermes_harness.gateway.gateway_contract_probe $FIX"
pair gateway-lookup-probe "" "$BIN/hermes-gateway-lookup-probe $FIX" \
  "$PY hermes_harness.gateway.gateway_lookup_probe $FIX"
pair harness-new-ticket dir "$BIN/hermes-harness --state $D/state new t-1 bench" \
  "$PY hermes_harness.cli --state $D/state new t-1 bench"
pair supervised-demo dir "$BIN/hermes-supervised-demo --root $D/demo" \
  "$PY hermes_harness.agents.supervised_demo --root $D/demo"
pair agents-assemble repo \
  "$BIN/hermes-native agents-assemble --confirm-offline --repo $REPO --plan $PLAN --state $D/state" \
  "$PY hermes_harness.native agents-assemble --confirm-offline --repo $REPO --plan $PLAN --state $D/state"

echo "results in $OUT"
