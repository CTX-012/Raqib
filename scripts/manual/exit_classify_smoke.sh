#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 3.5 — exit-reason classification.
#
# Verifies: when an AI-classified process exits via SIGSEGV under the
# `edge_monitor exec` wrapper, the resulting RunRecord.exit_reason is
# the `Segfault` variant rather than `UserSignal { signal: 11 }` or
# `Unknown`. Segfault is the cheapest classifier path to exercise on
# the dev box (no dmesg, no CUDA, no governor); the other variants —
# OOM (kernel + CUDA), CudaError, GovernorKill, CleanExit — are
# covered by unit tests.
#
# Why exec specifically: in headless mode edge_monitor only observes
# external processes via /proc and so cannot recover the kernel signal
# once the PID disappears. The exec wrapper forks the child itself,
# captures the wait status (WTERMSIG=11 here), and then the Tier 3.5
# classifier consumes that signal to map to ExitReason::Segfault.
#
# Exit codes:
#   0   PASS — at least one RunRecord has exit_reason == "Segfault".
#   1   FAIL — segfault occurred but exit_reason was something else.
#   77  SKIP — Python 3 unavailable.

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "SKIP: requires python3 to trigger a deterministic SIGSEGV." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-exit-classify-smoke-XXXX)"
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

WORKLOAD="$TEMP_HOME/segfaulter.py"
cat > "$WORKLOAD" <<'EOF'
import os, signal, time
# Brief lifetime so the exec wrapper has at least one tick of
# telemetry, then send SIGSEGV to ourselves. os.kill bypasses any
# Python-level signal handling, so the kernel delivers SIGSEGV and
# the parent's wait() sees WTERMSIG=11.
time.sleep(1)
os.kill(os.getpid(), signal.SIGSEGV)
EOF

echo "==> running edge_monitor exec on the segfaulter (will crash)"
"$BIN" --config "$CONF" exec --name segfaulter -- \
    "$PYTHON" "$WORKLOAD" \
    > "$TEMP_HOME/exec.log" 2>&1 || true

"$BIN" --config "$CONF" history segfaulter --json > "$TEMP_HOME/h.json"

if "$PYTHON" -c "
import json, sys
data = json.load(open('$TEMP_HOME/h.json'))
if not data:
    print('no records', file=sys.stderr); sys.exit(1)
def is_segfault(er):
    if er == 'Segfault' or er == 'segfault':
        return True
    if isinstance(er, dict):
        # Internally tagged as { kind: 'segfault' } today; keep the
        # PascalCase fallback in case the runtime ever switches the
        # serde tag style.
        return er.get('kind') in ('segfault', 'Segfault')
    return False
for r in data:
    er = r.get('exit_reason')
    if is_segfault(er):
        print(f'exit_reason={er}')
        sys.exit(0)
print('exit_reasons seen: ' + repr([r.get('exit_reason') for r in data]),
      file=sys.stderr)
sys.exit(1)
"; then
    echo "PASS"
    exit 0
else
    echo "FAIL: SIGSEGV exit was not classified as Segfault" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/h.json" >&2 || true
    echo "FAIL"
    exit 1
fi
