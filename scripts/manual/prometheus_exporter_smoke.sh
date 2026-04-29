#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 2.3 — Prometheus exporter.
#
# Verifies: when `[telemetry] prometheus_bind` is set, the runtime
# binds an HTTP listener on that address; GET /metrics returns
# 200 text/plain with at least the well-known
# `edge_monitor_processes_total` metric family present.
#
# This is the only Tier-2/3 smoke we can fully exercise on the dev box
# without external infrastructure — the exporter is self-contained.
#
# Exit codes:
#   0   PASS — /metrics responds 200 with the expected family present.
#   1   FAIL — bind succeeded but response missing or malformed.
#   77  SKIP — `curl` unavailable.

set -euo pipefail
cd "$(dirname "$0")/../.."

if ! command -v curl >/dev/null 2>&1; then
    echo "SKIP: requires curl on PATH to scrape /metrics." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-prom-smoke-XXXX)"
trap 'rm -rf "$TEMP_HOME"; jobs -p | xargs -r kill -9 2>/dev/null || true' EXIT

echo "==> building release binary"
cargo build --release --quiet
BIN="$PWD/target/release/edge_monitor"

# Find an unused localhost port. /dev/tcp lets bash test reachability
# without a netcat dependency.
pick_port() {
    local p
    for p in $(seq 19472 19572); do
        if ! (echo > "/dev/tcp/127.0.0.1/$p") 2>/dev/null; then
            echo "$p"; return 0
        fi
    done
    return 1
}
PORT="$(pick_port)" || { echo "FAIL: no free port in 19472-19572" >&2; exit 1; }

CONF="$TEMP_HOME/edge_monitor.toml"
cat > "$CONF" <<EOF
[runtime]
tick_interval_ms = 500

[storage]
run_store_path = "$TEMP_HOME/run_store"

[telemetry]
prometheus_bind = "127.0.0.1:$PORT"
EOF

echo "==> launching edge_monitor (background, will exit after 30 ticks)"
"$BIN" --config "$CONF" --no-ui --ticks 30 --dry-run \
    > "$TEMP_HOME/em.log" 2>&1 &
EM_PID=$!

# Wait up to 8 s for the listener to come up.
for i in $(seq 1 16); do
    if curl -fsS "http://127.0.0.1:$PORT/metrics" \
        -o "$TEMP_HOME/metrics.txt" 2>/dev/null; then
        break
    fi
    sleep 0.5
done

if ! [[ -s "$TEMP_HOME/metrics.txt" ]]; then
    echo "FAIL: /metrics never responded on :$PORT" >&2
    tail -20 "$TEMP_HOME/em.log" >&2
    kill -TERM "$EM_PID" 2>/dev/null || true
    exit 1
fi

# Validate Prometheus text exposition format basics + presence of a
# canonical edge_monitor metric family.
HEAD=$(head -c 200 "$TEMP_HOME/metrics.txt")
echo "==> /metrics first 200 bytes:"
echo "$HEAD"

if ! grep -q "^# HELP edge_monitor_processes_total" "$TEMP_HOME/metrics.txt"; then
    echo "FAIL: edge_monitor_processes_total HELP comment missing" >&2
    head -40 "$TEMP_HOME/metrics.txt" >&2
    kill -TERM "$EM_PID" 2>/dev/null || true
    exit 1
fi
if ! grep -q "^# TYPE edge_monitor_processes_total gauge" "$TEMP_HOME/metrics.txt"; then
    echo "FAIL: edge_monitor_processes_total TYPE gauge missing" >&2
    head -40 "$TEMP_HOME/metrics.txt" >&2
    kill -TERM "$EM_PID" 2>/dev/null || true
    exit 1
fi
if ! grep -qE "^edge_monitor_processes_total\{category=" "$TEMP_HOME/metrics.txt"; then
    echo "FAIL: edge_monitor_processes_total samples missing" >&2
    head -40 "$TEMP_HOME/metrics.txt" >&2
    kill -TERM "$EM_PID" 2>/dev/null || true
    exit 1
fi
# tick_count_total must be a counter and present
if ! grep -q "^# TYPE edge_monitor_tick_count_total counter" "$TEMP_HOME/metrics.txt"; then
    echo "FAIL: edge_monitor_tick_count_total counter type comment missing" >&2
    head -60 "$TEMP_HOME/metrics.txt" >&2
    kill -TERM "$EM_PID" 2>/dev/null || true
    exit 1
fi
if ! grep -qE "^edge_monitor_tick_count_total " "$TEMP_HOME/metrics.txt"; then
    echo "FAIL: edge_monitor_tick_count_total sample missing" >&2
    head -60 "$TEMP_HOME/metrics.txt" >&2
    kill -TERM "$EM_PID" 2>/dev/null || true
    exit 1
fi

kill -TERM "$EM_PID" 2>/dev/null || true
wait "$EM_PID" 2>/dev/null || true

echo "PASS"
exit 0
