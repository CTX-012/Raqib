#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 1.2d — stdout regex parser + exec wrapper.
#
# Verifies: `edge_monitor exec -- COMMAND` tees stdout through the
# `stdout_parser` sampler, populates `ExecStats`, and persists a
# `RunRecord` whose `metrics.tokens_per_sec_avg` is set when the
# wrapped command emits llama.cpp-style "eval time = X tokens per
# second" lines on stdout.
#
# This script is INTENTIONALLY self-contained: it generates the
# llama.cpp log lines from a small inline Python script, so the smoke
# test has no external runtime dependency beyond Python 3 and the
# release binary itself. Tier 1.2d is the only Tier-1 sampler we can
# fully exercise on the dev box without spinning up a heavy runtime.
#
# Exit codes:
#   0   PASS — RunRecord has tokens_per_sec_avg populated.
#   1   FAIL — exec ran but no tps was recorded.
#   77  SKIP — Python 3 not available.

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "SKIP: requires python3 on PATH for the synthetic stdout source." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-exec-smoke-XXXX)"
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
EOF

# Inline Python that emits 5 llama.cpp-style eval lines, one per
# second, then exits cleanly. The stdout regex parser should pick up
# tokens_per_sec on every line.
GEN_SCRIPT="$TEMP_HOME/fake_llama.py"
cat > "$GEN_SCRIPT" <<'EOF'
import sys, time
samples = [37.4, 38.9, 36.1, 39.2, 38.0]
for tps in samples:
    print(f"llama_print_timings: eval time = 1234.0 ms / 50 runs = "
          f"24.7 ms per token,  {tps:.1f} tokens per second")
    sys.stdout.flush()
    time.sleep(1)
EOF

echo "==> running edge_monitor exec -- python fake_llama.py"
"$BIN" --config "$CONF" exec --name fake-llama -- \
    "$PYTHON" "$GEN_SCRIPT" > "$TEMP_HOME/exec.log" 2>&1 || {
        echo "FAIL: exec subcommand exited non-zero" >&2
        tail -20 "$TEMP_HOME/exec.log" >&2
        exit 1
    }

echo "==> querying history --json fake-llama"
"$BIN" --config "$CONF" history fake-llama --json > "$TEMP_HOME/h.json"

if "$PYTHON" -c "
import json, sys
data = json.load(open('$TEMP_HOME/h.json'))
if not data:
    print('no records', file=sys.stderr); sys.exit(1)
metrics = (data[0].get('metrics') or {})
tps_avg = metrics.get('tokens_per_sec_avg')
tps_peak = metrics.get('tokens_per_sec_peak')
if tps_avg is None or tps_peak is None:
    print(f'tokens_per_sec_avg={tps_avg!r} tokens_per_sec_peak={tps_peak!r}',
          file=sys.stderr); sys.exit(1)
# Sanity: avg should be in the ballpark of the synthetic samples.
if not (30.0 <= tps_avg <= 45.0):
    print(f'tokens_per_sec_avg={tps_avg} outside 30..45 sanity range',
          file=sys.stderr); sys.exit(1)
print(f'tokens_per_sec_avg={tps_avg:.2f} tokens_per_sec_peak={tps_peak:.2f}')
"; then
    echo "PASS"
    exit 0
else
    echo "FAIL: stdout parser did not record tokens_per_sec on RunRecord" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/h.json" >&2 || true
    echo "FAIL"
    exit 1
fi
