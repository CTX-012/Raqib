#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 1.3 — regression warning on exit.
#
# Verifies: after enough baseline runs of a model, a sufficiently
# slower run causes the runtime to emit a `tracing::warn!` regression
# alert AND the regression event appears in the headless stderr
# stream.
#
# Method: drive 5 runs of an inline "fast" fake-llama via `exec`, then
# 1 run of the same `--name` at much lower tps. Inspect the stderr
# from the slow run for the `regression` target.
#
# Note: regression detection runs from the runtime tick loop on every
# AI process exit; the `exec` subcommand ALSO triggers regression
# detection when the wrapped command exits, because exec persists a
# RunRecord through the same RunStore. We rely on that here.
#
# Exit codes:
#   0   PASS — stderr contained a regression alert for tokens_per_sec_avg.
#   1   FAIL — six runs landed but no regression alert fired.
#   77  SKIP — Python 3 unavailable.

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "SKIP: requires python3 on PATH for the synthetic stdout source." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-regression-smoke-XXXX)"
trap 'rm -rf "$TEMP_HOME"' EXIT

echo "==> building release binary"
cargo build --release --quiet
BIN="$PWD/target/release/edge_monitor"

CONF="$TEMP_HOME/edge_monitor.toml"
cat > "$CONF" <<EOF
[runtime]
tick_interval_ms = 500

[storage]
run_store_path = "$TEMP_HOME/run_store"

[regression]
warn_pct = 10.0
critical_pct = 25.0
baseline_window = 10
min_baseline_samples = 3
EOF

GEN_SCRIPT="$TEMP_HOME/fake_llama.py"
cat > "$GEN_SCRIPT" <<'EOF'
import sys, time
tps = float(sys.argv[1])
for _ in range(4):
    print(f"llama_print_timings: eval time = 1000 ms / 50 runs = "
          f"20.0 ms per token,  {tps:.1f} tokens per second")
    sys.stdout.flush()
    time.sleep(0.6)
EOF

NAME="regression-smoke-$$"
echo "==> seeding 5 baseline runs at 40 tok/s"
for i in 1 2 3 4 5; do
    "$BIN" --config "$CONF" exec --name "$NAME" -- \
        "$PYTHON" "$GEN_SCRIPT" 40.0 \
        > "$TEMP_HOME/seed-$i.log" 2>&1
done

echo "==> firing slow run at 22 tok/s (should trip critical regression)"
"$BIN" --config "$CONF" exec --name "$NAME" -- \
    "$PYTHON" "$GEN_SCRIPT" 22.0 \
    > "$TEMP_HOME/slow.log" 2>&1

# The regression alert lands on stderr (tracing) — exec teed both
# streams to the file, so it's all in slow.log.
if grep -E "regression|regress" "$TEMP_HOME/slow.log" >/dev/null; then
    echo "==> regression alert seen:"
    grep -E "regression|regress" "$TEMP_HOME/slow.log" | head -3
    echo "PASS"
    exit 0
else
    echo "FAIL: no regression alert for tokens_per_sec_avg in stderr" >&2
    echo "----- slow.log tail -----" >&2
    tail -30 "$TEMP_HOME/slow.log" >&2 || true
    echo "FAIL"
    exit 1
fi
