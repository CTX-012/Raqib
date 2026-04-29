#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 1.2b — llama.cpp `llama-server` sampler.
#
# Verifies: when a real llama-server is running on :8080 with a real
# .gguf model loaded, the dispatcher detects it, scrapes its /metrics
# endpoint, and a tokens/sec value lands on the RunRecord (derived from
# llama_server_n_decode_total when no direct gauge is exposed).
#
# Skip preamble: llama-server is heavy. The script exits 77 when:
#   * `llama-server` is not on PATH OR
#   * LLAMACPP_SMOKE_MODEL is unset and no .gguf is found in CWD
#
# Exit codes:
#   0   PASS — sampler observed tokens/sec from llama-server
#   1   FAIL — llama-server was running but no tps was recorded
#   77  SKIP — prerequisites unmet

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"

if ! command -v llama-server >/dev/null 2>&1; then
    echo "SKIP: requires \`llama-server\` on PATH (the llama.cpp HTTP" >&2
    echo "      server). Tier 1.2b sampler cannot be exercised without" >&2
    echo "      a real /metrics endpoint at :8080." >&2
    exit 77
fi

MODEL="${LLAMACPP_SMOKE_MODEL:-}"
if [[ -z "$MODEL" ]]; then
    MODEL="$(find . -maxdepth 3 -name '*.gguf' -print -quit 2>/dev/null || true)"
fi
if [[ -z "$MODEL" || ! -f "$MODEL" ]]; then
    echo "SKIP: requires LLAMACPP_SMOKE_MODEL=<path-to-.gguf> set or a" >&2
    echo "      .gguf file in the working tree. Tier 1.2b cannot run" >&2
    echo "      without a real model to load." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-llamacpp-smoke-XXXX)"
PORT="${LLAMACPP_SMOKE_PORT:-8080}"
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
llamacpp_scrape = true
EOF

echo "==> starting llama-server -m $MODEL --port $PORT (background)"
llama-server -m "$MODEL" --port "$PORT" --host 127.0.0.1 \
    > "$TEMP_HOME/server.log" 2>&1 &
SERVER_PID=$!

for i in $(seq 1 60); do
    if curl -fsS "http://127.0.0.1:$PORT/metrics" >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

if ! curl -fsS "http://127.0.0.1:$PORT/metrics" >/dev/null 2>&1; then
    echo "FAIL: llama-server /metrics never came up on :$PORT" >&2
    tail -20 "$TEMP_HOME/server.log" >&2
    exit 1
fi

# Drive a brief completion so n_decode_total advances.
curl -fsS "http://127.0.0.1:$PORT/completion" \
    -H 'Content-Type: application/json' \
    -d '{"prompt":"hello","n_predict":8}' \
    > "$TEMP_HOME/completion.json" 2>&1 || true

echo "==> running edge_monitor headless for 15 ticks"
"$BIN" --config "$CONF" --no-ui --ticks 15 --dry-run \
    > "$TEMP_HOME/em.log" 2>&1 || {
        echo "FAIL: edge_monitor exited non-zero" >&2
        tail -20 "$TEMP_HOME/em.log" >&2
        exit 1
    }

kill -TERM "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true

echo "==> querying history --json"
"$BIN" --config "$CONF" history --json > "$TEMP_HOME/history.json"

if "$PYTHON" -c "
import json, sys
data = json.load(open('$TEMP_HOME/history.json'))
metrics = [(r.get('metrics') or {}) for r in data]
tps = [m.get('tokens_per_sec_avg') for m in metrics]
sys.exit(0 if any(t is not None for t in tps) else 1)
"; then
    echo "PASS"
    exit 0
else
    echo "FAIL: no run record with tokens_per_sec_avg observed" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/history.json" >&2 || true
    echo "FAIL"
    exit 1
fi
