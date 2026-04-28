#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 1.1 (per-model run history viewer).
#
# Drives the binary end-to-end:
#  1. Builds release.
#  2. Runs edge_monitor headless against a real workload (yolo_loop.py)
#     for a few ticks against a tempdir-backed RunStore.
#  3. Kills the workload.
#  4. Invokes `edge_monitor history` (no model + with model + --json)
#     and asserts each output mentions the workload's model.
#
# Skips gracefully on machines without ultralytics or yolov8n.pt.

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-/home/faisal/miniconda/bin/python}"
YOLO_SCRIPT="$PWD/yolo_loop.py"
TEMP_HOME="$(mktemp -d -t em-history-XXXX)"
trap 'rm -rf "$TEMP_HOME"' EXIT

if [[ ! -x "$PYTHON" ]] || ! "$PYTHON" -c "import ultralytics" 2>/dev/null; then
    echo "skip: ultralytics not available at $PYTHON" >&2
    exit 0
fi
if [[ ! -f "$YOLO_SCRIPT" ]]; then
    echo "skip: $YOLO_SCRIPT missing" >&2
    exit 0
fi

echo "==> building release binary"
cargo build --release --quiet
BIN="$PWD/target/release/edge_monitor"

CONF="$TEMP_HOME/edge_monitor.toml"
cat > "$CONF" <<EOF
[runtime]
tick_interval_ms = 1000

[storage]
run_store_path = "$TEMP_HOME/run_store"
EOF

echo "==> starting yolo workload"
"$PYTHON" "$YOLO_SCRIPT" > "$TEMP_HOME/yolo.log" 2>&1 &
YOLO_WRAPPER=$!
sleep 3
YOLO_PID="$(pgrep -P "$YOLO_WRAPPER" python | head -1 || true)"
if [[ -z "${YOLO_PID:-}" ]]; then
    YOLO_PID="$YOLO_WRAPPER"
fi
echo "    yolo PID=$YOLO_PID"

# Schedule the kill so the runtime captures the exit summary.
( sleep 6 && kill -TERM "$YOLO_PID" 2>/dev/null || true ) &

echo "==> running edge_monitor for 12 ticks"
"$BIN" --config "$CONF" --no-ui --ticks 12 --dry-run \
    > "$TEMP_HOME/em.log" 2>&1 || {
        echo "FAIL: edge_monitor exited non-zero" >&2
        tail -20 "$TEMP_HOME/em.log" >&2
        exit 1
    }

echo "==> 'history' (no model)"
"$BIN" --config "$CONF" history | tee "$TEMP_HOME/h-all.txt"
grep -q yolov8n "$TEMP_HOME/h-all.txt" || {
    echo "FAIL: history (no model) did not mention yolov8n" >&2
    exit 1
}

echo "==> 'history yolov8n'"
"$BIN" --config "$CONF" history yolov8n | tee "$TEMP_HOME/h-y.txt"
grep -q "Avg CPU" "$TEMP_HOME/h-y.txt" || {
    echo "FAIL: history yolov8n missing header" >&2
    exit 1
}

echo "==> 'history yolov8n --json'"
"$BIN" --config "$CONF" history yolov8n --json > "$TEMP_HOME/h-y.json"
"$PYTHON" -c "import json,sys; d=json.load(open('$TEMP_HOME/h-y.json')); assert d, 'empty'; assert d[0]['summary']['model_name']=='yolov8n'" || {
    echo "FAIL: history --json shape mismatch" >&2
    exit 1
}

echo "==> ALL OK"
