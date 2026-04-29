#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 3.7 — `edge_monitor compare` CLI.
#
# Verifies: after seeding a run store with two distinct named runs,
# `edge_monitor compare A B --runs N --json` returns a JSON array
# with one column per requested model and the column carries
# `tokens_per_sec_avg` populated from the seeded runs. The plain-
# text form of the same command is also expected to mention both
# model names in the header row.
#
# Exit codes:
#   0   PASS — JSON array has both columns + plain-text header
#             names both models.
#   1   FAIL — compare ran but its output is missing one or both.
#   77  SKIP — Python 3 unavailable.

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "SKIP: requires python3 for the synthetic stdout source." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-compare-smoke-XXXX)"
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

GEN="$TEMP_HOME/gen.py"
cat > "$GEN" <<'EOF'
import sys, time
tps = float(sys.argv[1])
for _ in range(3):
    print(f"llama_print_timings: eval time = 1000 ms / 50 runs = "
          f"20.0 ms per token,  {tps:.1f} tokens per second", flush=True)
    time.sleep(0.5)
EOF

NAME_A="cmp-fast-$$"
NAME_B="cmp-slow-$$"

echo "==> seeding 3 runs of $NAME_A at 40 tok/s"
for i in 1 2 3; do
    "$BIN" --config "$CONF" exec --name "$NAME_A" -- \
        "$PYTHON" "$GEN" 40.0 > "$TEMP_HOME/a-$i.log" 2>&1
done

echo "==> seeding 3 runs of $NAME_B at 18 tok/s"
for i in 1 2 3; do
    "$BIN" --config "$CONF" exec --name "$NAME_B" -- \
        "$PYTHON" "$GEN" 18.0 > "$TEMP_HOME/b-$i.log" 2>&1
done

echo "==> running compare --json"
"$BIN" --config "$CONF" compare "$NAME_A" "$NAME_B" --runs 3 --json \
    > "$TEMP_HOME/compare.json"

echo "==> running compare (text)"
"$BIN" --config "$CONF" compare "$NAME_A" "$NAME_B" --runs 3 \
    > "$TEMP_HOME/compare.txt"

JSON_OK=0
TEXT_OK=0
if "$PYTHON" -c "
import json, sys
data = json.load(open('$TEMP_HOME/compare.json'))
fast_name, slow_name = '$NAME_A', '$NAME_B'
names = [c.get('model') for c in data]
def tps(col):
    m = col.get('tokens_per_sec_avg')
    if isinstance(m, dict):
        return m.get('mean')
    return m
if fast_name in names and slow_name in names:
    fast = next(c for c in data if c.get('model') == fast_name)
    slow = next(c for c in data if c.get('model') == slow_name)
    f_tps = tps(fast)
    s_tps = tps(slow)
    print('fast={} slow={}'.format(f_tps, s_tps))
    sys.exit(0 if (f_tps and s_tps) else 1)
sys.exit(1)
"; then
    JSON_OK=1
fi

if grep -q "$NAME_A" "$TEMP_HOME/compare.txt" \
   && grep -q "$NAME_B" "$TEMP_HOME/compare.txt"; then
    TEXT_OK=1
fi

if [[ "$JSON_OK" -eq 1 && "$TEXT_OK" -eq 1 ]]; then
    echo "PASS"
    exit 0
else
    echo "FAIL: compare output missing one or both models" >&2
    echo "----- compare.json -----" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/compare.json" >&2 || true
    echo "----- compare.txt -----" >&2
    cat "$TEMP_HOME/compare.txt" >&2 || true
    echo "FAIL"
    exit 1
fi
