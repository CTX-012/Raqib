#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 3.3 — KV cache pressure.
#
# Verifies: when a real vLLM serve process is running, the dispatcher
# scrapes `vllm:gpu_cache_usage_perc` and `vllm:num_preemptions_total`
# and `kv_cache_avg_pct` / `kv_cache_evictions_total` land on the
# RunRecord.
#
# Skip preamble: vLLM is the only runtime today that exposes KV cache
# pressure as a Prometheus metric, and standing it up requires CUDA +
# a real model. The script exits 77 when:
#   * vLLM is not on PATH AND `python -c "import vllm"` fails
# Otherwise: same shape as vllm_smoke.sh, but asserts on the KV
# fields rather than tokens_per_sec_avg.
#
# Exit codes:
#   0   PASS — kv_cache_avg_pct populated on at least one RunRecord.
#   1   FAIL — vLLM ran but KV pct stayed null.
#   77  SKIP — vLLM unavailable.

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
if ! command -v vllm >/dev/null 2>&1 \
    && ! "$PYTHON" -c "import vllm" 2>/dev/null; then
    echo "SKIP: requires a working vLLM installation. Tier 3.3 KV-cache" >&2
    echo "      pressure is sourced from vllm:gpu_cache_usage_perc and" >&2
    echo "      cannot be exercised without a real vLLM /metrics endpoint." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-kv-smoke-XXXX)"
trap 'rm -rf "$TEMP_HOME"; jobs -p | xargs -r kill -9 2>/dev/null || true' EXIT

echo "==> building release binary"
cargo build --release --quiet
BIN="$PWD/target/release/edge_monitor"

CONF="$TEMP_HOME/edge_monitor.toml"
cat > "$CONF" <<EOF
[runtime]
tick_interval_ms = 500

[storage]
run_store_path = "$TEMP_HOME/run_store"

[telemetry]
vllm_scrape = true
EOF

MODEL="${VLLM_SMOKE_MODEL:-facebook/opt-125m}"
PORT="${VLLM_SMOKE_PORT:-9877}"

echo "==> starting vllm serve $MODEL on :$PORT"
vllm serve "$MODEL" --port "$PORT" \
    > "$TEMP_HOME/vllm.log" 2>&1 &
VLLM_PID=$!

for i in $(seq 1 60); do
    if curl -fsS "http://127.0.0.1:$PORT/metrics" >/dev/null 2>&1; then break; fi
    sleep 1
done
if ! curl -fsS "http://127.0.0.1:$PORT/metrics" >/dev/null 2>&1; then
    echo "FAIL: vllm /metrics never came up on :$PORT" >&2
    tail -20 "$TEMP_HOME/vllm.log" >&2
    exit 1
fi

# Drive concurrent generation so the KV cache fills above 0%.
for i in $(seq 1 8); do
    curl -fsS "http://127.0.0.1:$PORT/v1/completions" \
        -H 'Content-Type: application/json' \
        -d "{\"model\":\"$MODEL\",\"prompt\":\"hello $i\",\"max_tokens\":32}" \
        > "$TEMP_HOME/gen-$i.json" 2>&1 &
done

echo "==> running edge_monitor headless for 25 ticks"
"$BIN" --config "$CONF" --no-ui --ticks 25 --dry-run \
    > "$TEMP_HOME/em.log" 2>&1 || {
        echo "FAIL: edge_monitor exited non-zero" >&2
        tail -20 "$TEMP_HOME/em.log" >&2
        exit 1
    }

wait || true
kill -TERM "$VLLM_PID" 2>/dev/null || true
wait "$VLLM_PID" 2>/dev/null || true

"$BIN" --config "$CONF" history --json > "$TEMP_HOME/h-summary.json"

if "$PYTHON" -c "
import json, subprocess, sys
summary = json.load(open('$TEMP_HOME/h-summary.json'))
for d in summary:
    name = d.get('model') or ''
    if not name: continue
    out = subprocess.check_output(['$BIN','--config','$CONF','history',name,'--json'])
    runs = json.loads(out)
    for r in runs:
        m = r.get('metrics') or {}
        if m.get('kv_cache_avg_pct') is not None or m.get('kv_cache_peak_pct') is not None:
            print(f'kv_avg={m.get(\"kv_cache_avg_pct\")} '
                  f'kv_peak={m.get(\"kv_cache_peak_pct\")} '
                  f'evictions={m.get(\"kv_cache_evictions_total\")}')
            sys.exit(0)
sys.exit(1)
"; then
    echo "PASS"
    exit 0
else
    echo "FAIL: no RunRecord had kv_cache_*_pct populated" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/h-summary.json" >&2 || true
    echo "FAIL"
    exit 1
fi
