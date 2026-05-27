#!/usr/bin/env bash
# B2 production-shape harness — surfaces the v1.1.1 bash-child-invisibility bug.
#
# Confirms that a `bash` tool-child of a claude PID is classified NotAi
# and therefore absent from the AI-filtered slice (ai_procs), so a
# child-detecting sampler MUST read all_procs. A unit fixture that
# hands the bash child to the sampler directly cannot reproduce this —
# this is the v1.1.1 → v1.1.2 slice gap.
#
# Read-only. LOCAL-RUN ONLY — see README "CI status".
#
# Dependencies: a running edge_monitor on $PORT and at least one
# claude agent process the classifier recognises.
#
# Exit: 0 = slice gap confirmed (bash child absent from ai_procs,
#           present in all_procs — the condition v1.1.2 reads
#           all_procs to handle). 1 = missing deps / no claude agent /
#           unexpected classifier behaviour.
#
# Usage: PORT=7273 ./b2_agent_claude_harness.sh
set -euo pipefail
PORT="${PORT:-7273}"

fail() { echo "FAIL: $*" >&2; exit 1; }

command -v curl    >/dev/null || fail "curl not installed"
command -v python3 >/dev/null || fail "python3 not installed"
curl -fsS "http://127.0.0.1:$PORT/api/snapshot" >/dev/null 2>&1 \
  || fail "edge_monitor /api/snapshot unreachable at 127.0.0.1:$PORT (run edge_monitor --bind 127.0.0.1:$PORT)"

snap=$(curl -fsS "http://127.0.0.1:$PORT/api/snapshot")

# Find a claude agent PID from the live snapshot.
claude_pid=$(echo "$snap" | python3 -c "
import json,sys
d=json.load(sys.stdin)
a=[w['pid'] for w in d.get('workloads',[]) if w.get('workload_category')=='agent']
print(a[0] if a else '')
")
[[ -n "$claude_pid" ]] \
  || fail "no claude agent classified — is the classifier matching this host's claude path? (v1.1.1 extended SAAS_LLM_CLI_PATTERNS for local .vscode/extensions/)"
echo "claude agent PID: $claude_pid"

# Spawn a bash child of THIS shell, let it live briefly.
( sleep 4 ) &
child=$!
child_comm=$(cat "/proc/$child/comm" 2>/dev/null || echo "?")

# ai_procs equivalent: the snapshot's AI workloads. bash is NotAi, so
# the child must be ABSENT here.
in_ai=$(echo "$snap" | python3 -c "
import json,sys
d=json.load(sys.stdin)
print('TRUE' if any(w.get('pid')==$child for w in d.get('workloads',[])) else 'FALSE')
")
# all_procs equivalent: the full /proc list — the child is always here.
in_all=$([[ -d "/proc/$child" ]] && echo TRUE || echo FALSE)
kill "$child" 2>/dev/null || true
wait "$child" 2>/dev/null || true

echo "bash tool-child PID $child (comm=$child_comm):"
echo "  present in ai_procs slice (AI workloads only):  $in_ai"
echo "  present in all_procs slice (full /proc list):   $in_all"

if [[ "$in_ai" == "FALSE" && "$in_all" == "TRUE" ]]; then
  echo "PASS: bash child invisible to ai_procs, visible only in all_procs."
  echo "      A B2 reading ai_procs (v1.1.1 bug) sees zero children -> Idle."
  echo "      v1.1.2 reads all_procs -> correct. Slice gap confirmed."
  exit 0
fi
fail "unexpected slice state (in_ai=$in_ai in_all=$in_all) — investigate \
classifier / runtime-filter behaviour; the v1.1.2 premise (bash NotAi, \
absent from ai_procs) did not hold on this host."
