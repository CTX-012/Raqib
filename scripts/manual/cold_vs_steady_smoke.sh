#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 3.2 — cold-start vs steady-state separation.
#
# Verifies: when the cold-load detector (Tier 2.2) declares a model
# load complete mid-run, the post-watermark frames feed both the
# overall and the new `_steady` aggregates on the RunRecord, so a
# run that emits slow tps during cold load and fast tps after
# results in `tokens_per_sec_avg_steady > tokens_per_sec_avg_overall`.
#
# Why this is hard to exercise on the dev box: the cold-load
# detector runs from the headless tick loop's per-process
# /proc/<pid>/io scrape, NOT from the `edge_monitor exec` wrapper.
# So a usable smoke needs a real LLM serving runtime that produces
# (a) a disk-burst-then-plateau shape that fires cold-load AND
# (b) a Prometheus / API source feeding tokens-per-second into the
# accumulator simultaneously. A synthetic stdout-only workload
# under exec cannot exercise the watermark — see Cross-builder
# requests in BUILDER_STATUS.md ("exec wrapper does not run the
# Tier 2.2 cold-load tracker").
#
# Exit codes:
#   0   PASS — both _avg and _avg_steady populated, _steady >= _avg.
#   1   FAIL — both fields populated but the watermark logic is wrong.
#   77  SKIP — no LLM runtime present, OR the exec→cold_load
#             wiring is still absent (the dev-box default).

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "SKIP: requires python3 on PATH." >&2
    exit 77
fi

if ! command -v vllm >/dev/null 2>&1 \
    && ! command -v llama-server >/dev/null 2>&1 \
    && ! curl -fsS "${OLLAMA_HOST:-127.0.0.1:11434}/api/ps" >/dev/null 2>&1; then
    echo "SKIP: requires a real LLM runtime (vLLM / llama-server /" >&2
    echo "      Ollama daemon) so the headless tick loop sees both a" >&2
    echo "      Tier 2.2 cold-load disk burst AND a tokens-per-sec" >&2
    echo "      source feeding the accumulator. The \`edge_monitor exec\`" >&2
    echo "      wrapper does not run the cold-load tracker today (see" >&2
    echo "      BUILDER_STATUS.md cross-builder requests), so a" >&2
    echo "      synthetic stdout-only workload cannot exercise the" >&2
    echo "      steady-state watermark transition." >&2
    exit 77
fi
# Once a real LLM runtime is on the box, this smoke needs site-
# specific configuration (model path, port, expected tps). Bail
# out with SKIP rather than papering over the gap.
echo "SKIP: an LLM runtime was detected, but this smoke needs site-" >&2
echo "      specific model + port wiring before it can drive a real" >&2
echo "      cold-load + steady-state run. Add it once a fleet" >&2
echo "      operator picks a representative workload." >&2
exit 77

TEMP_HOME="$(mktemp -d -t em-cold-steady-smoke-XXXX)"
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

# Build a 64 MiB synthetic .gguf and a workload that:
#   1. reads it sequentially (cold-load burst)
#   2. emits 4 lines of "slow" tps in parallel (these get tagged into
#      pre-steady-state aggregates)
#   3. waits long enough for the plateau detector to fire
#   4. emits 6 lines of "fast" tps (post-steady-state)
WEIGHTS="$TEMP_HOME/fake.gguf"
dd if=/dev/urandom of="$WEIGHTS" bs=1M count=64 status=none

WORKLOAD="$TEMP_HOME/two_phase.py"
cat > "$WORKLOAD" <<'EOF'
import sys, time, threading

# Phase 1: read whole 64 MiB file, while emitting slow tps lines.
def emit(values, gap):
    for tps in values:
        print(f"llama_print_timings: eval time = 1000 ms / 50 runs = "
              f"20.0 ms per token,  {tps:.1f} tokens per second", flush=True)
        time.sleep(gap)

path = None
i = 1
while i < len(sys.argv):
    if sys.argv[i] == '--model' and i+1 < len(sys.argv):
        path = sys.argv[i+1]; break
    i += 1
assert path, 'no --model'

slow = [12.0, 12.5, 11.8, 13.0]
fast = [42.0, 41.5, 43.0, 42.5, 41.0, 42.8]

t = threading.Thread(target=emit, args=(slow, 0.5))
t.start()
total = 0
with open(path, 'rb') as f:
    while True:
        chunk = f.read(1 << 20)
        if not chunk: break
        total += len(chunk)
print(f'read={total}', flush=True)
t.join()

# Wait long enough that the cold-load plateau detector fires
# (PLATEAU_TICKS=2 at 500 ms tick_interval = ~1 s; give it 3 s).
time.sleep(3)

emit(fast, 0.5)
EOF

echo "==> running edge_monitor exec on the two-phase workload"
"$BIN" --config "$CONF" exec --name two-phase -- \
    "$PYTHON" "$WORKLOAD" --model "$WEIGHTS" \
    > "$TEMP_HOME/exec.log" 2>&1 || {
        echo "FAIL: exec exited non-zero" >&2
        tail -20 "$TEMP_HOME/exec.log" >&2
        exit 1
    }

"$BIN" --config "$CONF" history two-phase --json > "$TEMP_HOME/h.json"

if "$PYTHON" -c "
import json, sys
data = json.load(open('$TEMP_HOME/h.json'))
if not data:
    print('no records', file=sys.stderr); sys.exit(1)
m = data[0].get('metrics') or {}
overall = m.get('tokens_per_sec_avg')
steady  = m.get('tokens_per_sec_avg_steady')
print(f'overall={overall} steady={steady}')
if overall is None or steady is None:
    print('overall or steady is None', file=sys.stderr); sys.exit(1)
if steady < overall:
    print(f'steady ({steady}) < overall ({overall}) — watermark not respected',
          file=sys.stderr); sys.exit(1)
"; then
    echo "PASS"
    exit 0
else
    echo "FAIL: steady-state metric missing or not greater than overall" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/h.json" >&2 || true
    echo "FAIL"
    exit 1
fi
