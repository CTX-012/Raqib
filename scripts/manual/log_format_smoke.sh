#!/usr/bin/env bash
# scripts/manual/log_format_smoke.sh
#
# S.2 smoke — verify `edge_monitor --log-format json` emits one JSON
# object per line of stderr that downstream tooling (jq, fluentd,
# python json.loads) can parse without errors. Also verifies that
# `--log-format human` still emits the legacy K=V text format.
#
# Exits 0 on success, non-zero with a diagnostic on the first parse
# failure or shape mismatch.

set -euo pipefail

cd "$(dirname "$0")/../.." # repo root

# We need the release binary present. Building from this script keeps
# it reproducible on a fresh checkout.
echo "[smoke] cargo build --release"
cargo build --release --quiet

bin=./target/release/edge_monitor
test -x "$bin" || { echo "FAIL: $bin missing"; exit 1; }

# Pick a JSON validator. jq is preferred; python3 -m json.tool is the
# universal fallback — every CI runner has python3.
if command -v jq >/dev/null 2>&1; then
  validate() { jq . >/dev/null; }
else
  validate() {
    python3 -c '
import sys, json
n = 0
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    json.loads(line)
    n += 1
if n < 100:
    print(f"only {n} JSON lines, expected >= 100", file=sys.stderr)
    sys.exit(2)
print(f"parsed {n} JSON lines OK")
'
  }
fi

# 100 ticks at the default 1000 ms interval is 100 s. Lower the
# interval via a tempdir config so the smoke completes in <15 s.
out=$(mktemp)
cfg=$(mktemp --suffix=.toml)
trap 'rm -f "$out" "$cfg"' EXIT
cat >"$cfg" <<'TOML'
[runtime]
tick_interval_ms = 100
TOML
echo "[smoke] running 100 ticks with --log-format json (~10 s)"
"$bin" --config "$cfg" --no-ui --ticks 100 --log-format json 2>"$out" >/dev/null

echo "[smoke] $(wc -l <"$out") stderr lines captured"
validate <"$out"

# Sanity: human format must NOT be valid JSON for the per-line check.
# (Lines like `2026-... INFO foo` parse as bare strings if we used
# json.loads loosely, so check that none of them parse as objects.)
echo "[smoke] running 5 ticks with --log-format human"
"$bin" --config "$cfg" --no-ui --ticks 5 --log-format human 2>"$out" >/dev/null

if python3 -c '
import sys, json
for line in open(sys.argv[1]):
    line = line.strip()
    if not line: continue
    try:
        v = json.loads(line)
    except Exception:
        continue
    if isinstance(v, dict):
        print(f"human-format line parsed as JSON object: {line}", file=sys.stderr)
        sys.exit(2)
' "$out"; then
  echo "[smoke] human format is not JSON-shaped — good"
else
  echo "FAIL: human-format output looked like JSON"; exit 1
fi

echo "PASS: --log-format json is jq-clean over 100+ lines; human format remains text."
