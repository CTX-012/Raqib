#!/usr/bin/env bash
# B1 production-shape harness — surfaces the v1.1.0 digest-vs-name bug
# and validates the v1.1.1 fix against a REAL ollama runner.
#
# Spawns a real ollama runner, captures the model identity TWO ways:
#   (a) as edge_monitor's classifier surfaces it (from the runner's
#       --model cmdline path → a blob digest), via /api/snapshot
#   (b) as ollama's /api/ps reports it (the human-readable name)
# These DIFFER — the exact asymmetry that locked B1 to not_detected
# in v1.1.0. A symmetric unit fixture (same string both sides) cannot
# reproduce it. The harness then reads the runner's `activity` from
# the live snapshot and asserts it is NOT `not_detected` (the v1.1.1
# fix reconciled the asymmetry via /api/ps presence).
#
# Read-only against src/. LOCAL-RUN ONLY — see README "CI status".
#
# Dependencies: a running ollama daemon (localhost:11434) and a
# running edge_monitor on $PORT.
#
# Exit: 0 = ran and the runner reported a non-not_detected activity
#           (B1 fix working). 1 = missing deps / no runner / runner
#           stuck at not_detected (the v1.1.0 regression).
#
# Usage: PORT=7273 MODEL=smollm:135m ./b1_ollama_harness.sh
set -euo pipefail
PORT="${PORT:-7273}"
MODEL="${MODEL:-smollm:135m}"

fail() { echo "FAIL: $*" >&2; exit 1; }

command -v curl    >/dev/null || fail "curl not installed"
command -v python3 >/dev/null || fail "python3 not installed"
curl -fsS "http://localhost:11434/api/ps" >/dev/null 2>&1 \
  || fail "ollama daemon unreachable at localhost:11434 (start it, then retry)"
curl -fsS "http://127.0.0.1:$PORT/api/snapshot" >/dev/null 2>&1 \
  || fail "edge_monitor /api/snapshot unreachable at 127.0.0.1:$PORT (run edge_monitor --bind 127.0.0.1:$PORT)"

# Load the model so a runner subprocess exists.
curl -s http://localhost:11434/api/generate \
  -d "{\"model\":\"$MODEL\",\"prompt\":\"hi\",\"keep_alive\":\"2m\",\"stream\":false}" > /dev/null
sleep 3

snap=$(curl -fsS "http://127.0.0.1:$PORT/api/snapshot")

# (a) what edge_monitor classified the runner's model_name as
classified=$(echo "$snap" | python3 -c "
import json,sys
d=json.load(sys.stdin)
r=[w for w in d.get('workloads',[]) if w.get('workload_category')=='llm' and w.get('name')=='ollama' and w.get('model_name')]
print(r[0]['model_name'] if r else 'NONE')
")
# (b) what /api/ps calls it
apiname=$(curl -s http://localhost:11434/api/ps | python3 -c "
import json,sys
d=json.load(sys.stdin)
print(d['models'][0]['name'] if d.get('models') else 'NONE')
")
# the runner's activity state (the v1.1.1 fix signal)
activity=$(echo "$snap" | python3 -c "
import json,sys
d=json.load(sys.stdin)
r=[w for w in d.get('workloads',[]) if w.get('workload_category')=='llm' and w.get('name')=='ollama']
print(r[0].get('activity') or 'NONE' if r else 'NONE')
")

echo "runner model_name (as classified):  $classified"
echo "/api/ps model name:                 $apiname"
echo "runner activity (v1.1.1 signal):    $activity"

if [[ "$classified" != "$apiname" ]]; then
  echo "DIAGNOSTIC: classified != api — this is the v1.1.0 digest-vs-name"
  echo "            asymmetry. A naive '==' matcher never fires here."
fi

# v1.1.1 fix assertion: the runner must NOT be stuck at not_detected.
if [[ "$activity" == "not_detected" ]]; then
  fail "ollama runner activity is 'not_detected' despite a loaded model — \
the v1.1.0 B1 bug has regressed (the asymmetric matcher is back)."
fi
if [[ "$activity" == "NONE" ]]; then
  fail "no ollama runner found in the snapshot (model didn't load, or the \
classifier isn't matching the runner on this host)."
fi
echo "PASS: ollama runner activity is '$activity' (B1 reconciled the asymmetry)."
exit 0
