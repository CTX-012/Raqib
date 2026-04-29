#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 1.2a — vLLM Prometheus sampler.
#
# Verifies: when a real vLLM serve process is running, the dispatcher
# detects it (via cmdline / VLLM_* env), scrapes its /metrics endpoint,
# and tokens_per_sec_avg lands on the RunRecord.
#
# Skip preamble: vLLM is too heavy to spin up on every developer box
# (needs CUDA + a multi-GB model). The script exits with the standard
# "skip" code 77 when:
#   * `vllm` is not on PATH AND `python -c "import vllm"` fails
# This is by design — the script must NOT silently pass on hosts that
# can't actually exercise the sampler.
#
# Exit codes:
#   0   sampler observed tokens_per_sec on a real vLLM run
#   1   FAIL — vLLM was running but the sampler did not record tok/s
#   77  SKIP — no vLLM available

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"

if ! command -v vllm >/dev/null 2>&1 \
    && ! "$PYTHON" -c "import vllm" 2>/dev/null; then
    echo "SKIP: requires a working vLLM installation (vllm on PATH or" >&2
    echo "      'python -c \"import vllm\"' importable). Tier 1.2a sampler" >&2
    echo "      cannot be exercised without a real vLLM /metrics endpoint." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-vllm-smoke-XXXX)"
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

# Smallest model we know vLLM can chew on quickly for a smoke test.
# Override with VLLM_SMOKE_MODEL=org/repo if the box has a different
# tiny model on disk.
MODEL="${VLLM_SMOKE_MODEL:-facebook/opt-125m}"
PORT="${VLLM_SMOKE_PORT:-9876}"

echo "==> starting vllm serve $MODEL on :$PORT (background)"
vllm serve "$MODEL" --port "$PORT" \
    > "$TEMP_HOME/vllm.log" 2>&1 &
VLLM_PID=$!

# Wait up to 60 s for /metrics to come up.
for i in $(seq 1 60); do
    if curl -fsS "http://127.0.0.1:$PORT/metrics" >/dev/null 2>&1; then
        break
    fi
    sleep 1
done

if ! curl -fsS "http://127.0.0.1:$PORT/metrics" >/dev/null 2>&1; then
    echo "FAIL: vllm /metrics never came up on :$PORT" >&2
    tail -20 "$TEMP_HOME/vllm.log" >&2
    exit 1
fi

# Drive a tiny request so vLLM emits a non-zero throughput.
curl -fsS "http://127.0.0.1:$PORT/v1/completions" \
    -H 'Content-Type: application/json' \
    -d "{\"model\":\"$MODEL\",\"prompt\":\"hello\",\"max_tokens\":8}" \
    > "$TEMP_HOME/completion.json" 2>&1 || true

echo "==> running edge_monitor headless for 15 ticks"
"$BIN" --config "$CONF" --no-ui --ticks 15 --dry-run \
    > "$TEMP_HOME/em.log" 2>&1 || {
        echo "FAIL: edge_monitor exited non-zero" >&2
        tail -20 "$TEMP_HOME/em.log" >&2
        exit 1
    }

kill -TERM "$VLLM_PID" 2>/dev/null || true
wait "$VLLM_PID" 2>/dev/null || true

echo "==> querying history --json"
"$BIN" --config "$CONF" history --json > "$TEMP_HOME/history.json"

# We don't know the model_name vLLM will report (depends on cmdline);
# any record with non-null tokens_per_sec_avg is enough to prove the
# sampler was working.
if "$PYTHON" -c "
import json, sys
data = json.load(open('$TEMP_HOME/history.json'))
hits = [r for r in data if (r.get('summary') or {}).get('model_name')]
tps = [(r.get('metrics') or {}).get('tokens_per_sec_avg') for r in hits]
nonnull = [t for t in tps if t is not None]
sys.exit(0 if nonnull else 1)
"; then
    echo "PASS"
    exit 0
else
    echo "FAIL: no run record with tokens_per_sec_avg observed" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/history.json" >&2 || true
    echo "FAIL"
    exit 1
fi
