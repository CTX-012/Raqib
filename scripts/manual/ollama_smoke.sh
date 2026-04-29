#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 1.2c — Ollama /api/ps sampler.
#
# Verifies: when an `ollama` daemon is running with at least one model
# loaded, the dispatcher reaches /api/ps and promotes the model name
# onto the RunRecord even when the classifier sees only `ollama runner`.
#
# Skip preamble: requires a running Ollama daemon (`ollama serve`) on
# the default port 11434 with at least one model already pulled. The
# script exits 77 when:
#   * no daemon answers GET /api/ps, OR
#   * no model is loaded (the response has an empty `models` list)
#
# Exit codes:
#   0   PASS — sampler observed at least one loaded model on /api/ps
#             AND edge_monitor's history --json contains a record
#             whose model_name matches one of those Ollama models.
#   1   FAIL — daemon and a loaded model exist, but the sampler did
#             not promote a matching model_name onto the RunRecord.
#   77  SKIP — daemon unreachable or no model loaded.

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
OLLAMA_HOST="${OLLAMA_HOST:-127.0.0.1:11434}"

PS_JSON="$(curl -fsS "http://$OLLAMA_HOST/api/ps" 2>/dev/null || true)"
if [[ -z "$PS_JSON" ]]; then
    echo "SKIP: no Ollama daemon on http://$OLLAMA_HOST/. Tier 1.2c" >&2
    echo "      sampler cannot be exercised without a real /api/ps" >&2
    echo "      endpoint." >&2
    exit 77
fi

MODELS="$(echo "$PS_JSON" | "$PYTHON" -c '
import json, sys
data = json.load(sys.stdin)
for m in data.get("models", []):
    name = m.get("name") or m.get("model") or ""
    if name:
        print(name)
')"
if [[ -z "$MODELS" ]]; then
    echo "SKIP: Ollama daemon up but no models are loaded — pull and" >&2
    echo "      run one (e.g. \`ollama run phi3:mini\`) before re-running." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-ollama-smoke-XXXX)"
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

[telemetry]
ollama_api = true
EOF

# Drive the daemon so a runner process exists across the window.
FIRST_MODEL="$(echo "$MODELS" | head -1)"
echo "==> driving generation against $FIRST_MODEL"
( curl -fsS "http://$OLLAMA_HOST/api/generate" \
    -d "{\"model\":\"$FIRST_MODEL\",\"prompt\":\"hi\",\"stream\":false}" \
    > "$TEMP_HOME/gen.json" 2>&1 || true ) &

echo "==> running edge_monitor headless for 20 ticks"
"$BIN" --config "$CONF" --no-ui --ticks 20 --dry-run \
    > "$TEMP_HOME/em.log" 2>&1 || {
        echo "FAIL: edge_monitor exited non-zero" >&2
        tail -20 "$TEMP_HOME/em.log" >&2
        exit 1
    }

wait || true

echo "==> querying history --json (no model — full per-model summary)"
"$BIN" --config "$CONF" history --json > "$TEMP_HOME/h-summary.json"

# We expect at least one model_name from the loaded set to appear.
if "$PYTHON" -c "
import json, sys
loaded = set(open('$TEMP_HOME/loaded.txt').read().split() if False else '''$MODELS'''.split())
data = json.load(open('$TEMP_HOME/h-summary.json'))
seen = {(d.get('model') or '') for d in data}
hit = seen & loaded
sys.exit(0 if hit else 1)
"; then
    echo "PASS"
    exit 0
else
    echo "FAIL: no Ollama-loaded model name appeared in history --json" >&2
    echo "Loaded models: $MODELS" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/h-summary.json" >&2 || true
    echo "FAIL"
    exit 1
fi
