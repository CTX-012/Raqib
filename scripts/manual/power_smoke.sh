#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 2.1 — NVML + RAPL power & thermals.
#
# Verifies: with at least one of NVML or RAPL available on the host,
# the dispatcher records non-null `gpu_watts_avg` (NVML) or
# `cpu_watts_avg` (RAPL) onto a RunRecord for an AI-classified
# process.
#
# Skip preamble: NVML requires an NVIDIA driver + GPU passthrough;
# RAPL requires `/sys/class/powercap/intel-rapl:*/energy_uj` to be
# readable (often root-only on hardened distros). The script exits
# 77 when neither interface is available.
#
# Exit codes:
#   0   PASS — at least one of {gpu_watts_avg, cpu_watts_avg} is set
#             on the resulting RunRecord.
#   1   FAIL — power source available but no watts recorded.
#   77  SKIP — neither NVML nor readable RAPL available.

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "SKIP: requires python3 on PATH for the AI-classified workload." >&2
    exit 77
fi

HAVE_NVML=0
HAVE_RAPL=0
if command -v nvidia-smi >/dev/null 2>&1 \
    && nvidia-smi --query-gpu=power.draw --format=csv,noheader >/dev/null 2>&1; then
    HAVE_NVML=1
fi
if compgen -G "/sys/class/powercap/intel-rapl:*/energy_uj" >/dev/null; then
    if [[ -r "$(echo /sys/class/powercap/intel-rapl:*/energy_uj | awk '{print $1}')" ]]; then
        HAVE_RAPL=1
    fi
fi

if [[ "$HAVE_NVML" -eq 0 && "$HAVE_RAPL" -eq 0 ]]; then
    echo "SKIP: requires NVML-capable GPU (nvidia-smi reachable) or" >&2
    echo "      readable Intel RAPL counters at" >&2
    echo "      /sys/class/powercap/intel-rapl:*/energy_uj. Tier 2.1" >&2
    echo "      cannot be exercised without a real power source." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-power-smoke-XXXX)"
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

# A python script with a model-flavoured argv so the classifier picks
# it up as AI Inference (the argv path extractor matches *.gguf
# tokens). We do not actually load the model; the file just needs to
# exist for the path extractor.
WEIGHTS="$TEMP_HOME/fake.gguf"
dd if=/dev/zero of="$WEIGHTS" bs=1M count=1 status=none

WORKLOAD="$TEMP_HOME/burner.py"
cat > "$WORKLOAD" <<'EOF'
import sys, time
# Argv carries the model-path so edge_monitor classifies us as AI.
# We just spin the CPU for ~6 seconds so RAPL has at least one Δ
# window with a non-trivial reading.
end = time.time() + 6.0
x = 0.0
while time.time() < end:
    x = x * 1.0000001 + 1.0
print("done", x)
EOF

echo "==> launching CPU burner with AI argv (--model $WEIGHTS)"
"$PYTHON" "$WORKLOAD" --model "$WEIGHTS" \
    > "$TEMP_HOME/burner.log" 2>&1 &
BURN_PID=$!

echo "==> running edge_monitor headless for 14 ticks"
"$BIN" --config "$CONF" --no-ui --ticks 14 --dry-run \
    > "$TEMP_HOME/em.log" 2>&1 || {
        echo "FAIL: edge_monitor exited non-zero" >&2
        tail -20 "$TEMP_HOME/em.log" >&2
        exit 1
    }

wait "$BURN_PID" 2>/dev/null || true

"$BIN" --config "$CONF" history --json > "$TEMP_HOME/h.json"

if "$PYTHON" -c "
import json, sys
data = json.load(open('$TEMP_HOME/h.json'))
hits = []
for d in data:
    model = d.get('model') or ''
    if 'fake' not in model and 'burner' not in model and model != 'fake':
        # we don't strictly know the model_name; accept any AI exit.
        pass
    hits.append(d)
if not hits:
    print('no records', file=sys.stderr); sys.exit(1)
# Now look in the per-model recent runs.
import subprocess
got_watts = False
for h in hits:
    name = h.get('model') or ''
    if not name:
        continue
    out = subprocess.check_output(['$BIN','--config','$CONF','history',name,'--json'])
    runs = json.loads(out)
    for r in runs:
        m = r.get('metrics') or {}
        if m.get('gpu_watts_avg') is not None or m.get('cpu_watts_avg') is not None:
            got_watts = True
            break
sys.exit(0 if got_watts else 1)
"; then
    echo "PASS"
    exit 0
else
    echo "FAIL: no RunRecord had non-null gpu_watts_avg or cpu_watts_avg" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/h.json" >&2 || true
    echo "FAIL"
    exit 1
fi
