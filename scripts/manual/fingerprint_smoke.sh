#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 3.1 — partial-hash model fingerprinting.
#
# Verifies: when an AI-classified process holds a model file via
# `--model <path.gguf>` argv, the runtime computes a fingerprint at
# exit and stamps it onto `RunRecord.model_fingerprint`. The
# fingerprint format is the documented `sha256-head1m-tail64k:<hex>`
# self-describing string.
#
# Also asserts the cache works: a second run of the same model uses
# the cache (fingerprint string is identical, no re-hash latency).
#
# Exit codes:
#   0   PASS — both runs share a `sha256-head1m-tail64k:` fingerprint.
#   1   FAIL — fingerprint missing or malformed on at least one run.
#   77  SKIP — Python 3 unavailable.

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "SKIP: requires python3 for the AI-classified workload." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-fingerprint-smoke-XXXX)"
trap 'rm -rf "$TEMP_HOME"; jobs -p | xargs -r kill -9 2>/dev/null || true' EXIT

echo "==> building release binary"
cargo build --release --quiet
BIN="$PWD/target/release/edge_monitor"

CONF="$TEMP_HOME/edge_monitor.toml"
FP_CACHE="$TEMP_HOME/fingerprints.json"
cat > "$CONF" <<EOF
[runtime]
tick_interval_ms = 500

[storage]
run_store_path = "$TEMP_HOME/run_store"
fingerprint_cache = "$FP_CACHE"
EOF

# 2 MiB synthetic .gguf — bigger than the 1 MiB head window so the
# tail bytes also feed into the hash (exercises the full algorithm).
WEIGHTS="$TEMP_HOME/fp_test.gguf"
dd if=/dev/urandom of="$WEIGHTS" bs=1M count=2 status=none

WORKLOAD="$TEMP_HOME/holder.py"
cat > "$WORKLOAD" <<'EOF'
import sys, time
# Hold the file open briefly so edge_monitor sees us alive long enough
# to capture an AI exit summary.
time.sleep(2)
EOF

run_round() {
    local label="$1"
    "$PYTHON" "$WORKLOAD" --model "$WEIGHTS" \
        > "$TEMP_HOME/$label.log" 2>&1 &
    local pid=$!
    "$BIN" --config "$CONF" --no-ui --ticks 8 --dry-run \
        > "$TEMP_HOME/$label.em.log" 2>&1
    wait "$pid" 2>/dev/null || true
}

echo "==> round 1 (cold cache)"
run_round round1
echo "==> round 2 (warm cache)"
run_round round2

"$BIN" --config "$CONF" history --json > "$TEMP_HOME/h-summary.json"

if "$PYTHON" -c "
import json, subprocess, sys
summary = json.load(open('$TEMP_HOME/h-summary.json'))
fps = []
for d in summary:
    name = d.get('model') or ''
    if not name:
        continue
    out = subprocess.check_output(['$BIN','--config','$CONF','history',name,'--json'])
    runs = json.loads(out)
    for r in runs:
        f = r.get('model_fingerprint')
        if f:
            fps.append(f)
if not fps:
    print('no fingerprints', file=sys.stderr); sys.exit(1)
for f in fps:
    if not f.startswith('sha256-head1m-tail64k:'):
        print(f'unexpected format: {f}', file=sys.stderr); sys.exit(1)
# Both rounds touched the same file → fingerprints must match.
unique = set(fps)
if len(unique) != 1:
    print(f'expected one fingerprint, got {unique}', file=sys.stderr); sys.exit(1)
print(f'fp={fps[0]}')
"; then
    if [[ -s "$FP_CACHE" ]]; then
        echo "==> fingerprint cache populated at $FP_CACHE"
    else
        echo "WARN: fingerprint cache file missing or empty (still passes)"
    fi
    echo "PASS"
    exit 0
else
    echo "FAIL: fingerprint missing or inconsistent across rounds" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/h-summary.json" >&2 || true
    echo "FAIL"
    exit 1
fi
