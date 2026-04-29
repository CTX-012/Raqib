#!/usr/bin/env bash
# scripts/manual/sigterm_smoke.sh
#
# S.0.8 smoke — start `edge_monitor --no-ui --ticks 0` in the background,
# kill -TERM it, and verify:
#   * exit code 0 (not 143 — that's the default-action signal exit)
#   * the "shutdown requested" log line appears
#   * the "shutdown signal received; exiting" log line appears
#   * no orphaned children of the wrapper PID remain
#
# Exits 0 on success, non-zero with diagnostics on the first failure.

set -euo pipefail

cd "$(dirname "$0")/../.." # repo root

echo "[smoke] cargo build --release"
cargo build --release --quiet

bin=./target/release/edge_monitor
test -x "$bin" || { echo "FAIL: $bin missing"; exit 1; }

log=$(mktemp)
cfg=$(mktemp --suffix=.toml)
trap 'rm -f "$log" "$cfg"' EXIT
cat >"$cfg" <<'TOML'
[runtime]
tick_interval_ms = 200
TOML

echo "[smoke] starting headless edge_monitor in background"
"$bin" --config "$cfg" --no-ui --ticks 0 --log-format json 2>"$log" >/dev/null &
pid=$!
echo "[smoke] pid=$pid; sleeping 1s to let at least one tick complete"
sleep 1

# Capture children before SIGTERM so we can confirm they all clean up.
mapfile -t pre_children < <(ps --ppid "$pid" -o pid= 2>/dev/null || true)
echo "[smoke] children before SIGTERM: ${pre_children[*]:-(none)}"

echo "[smoke] sending SIGTERM"
kill -TERM "$pid"

# wait until child exits OR timeout
deadline=$(( $(date +%s) + 5 ))
while kill -0 "$pid" 2>/dev/null; do
  if [[ $(date +%s) -ge $deadline ]]; then
    echo "FAIL: edge_monitor did not exit within 5s of SIGTERM"
    kill -KILL "$pid" 2>/dev/null || true
    exit 1
  fi
  sleep 0.1
done

# `wait` also reports the exit code — set +e because wait can fail if
# the process is already reaped.
set +e
wait "$pid"
exit_code=$?
set -e
echo "[smoke] exit_code=$exit_code"

if [[ "$exit_code" -ne 0 ]]; then
  echo "FAIL: expected exit 0, got $exit_code (143 = default SIGTERM action; the handler did not catch the signal)"
  echo "--- log tail ---"
  tail -20 "$log"
  exit 1
fi

if ! grep -q '"message":"shutdown requested' "$log"; then
  echo "FAIL: missing 'shutdown requested' log line"
  cat "$log"; exit 1
fi
if ! grep -q '"message":"shutdown signal received' "$log"; then
  echo "FAIL: missing 'shutdown signal received; exiting' log line"
  cat "$log"; exit 1
fi

# Orphaned-child check: any pre-existing child PIDs still alive?
for c in "${pre_children[@]:-}"; do
  [[ -z "$c" ]] && continue
  if kill -0 "$c" 2>/dev/null; then
    echo "FAIL: child PID $c outlived the parent"
    exit 1
  fi
done

echo "PASS: SIGTERM → exit 0, drain logs present, no orphan children."
