#!/usr/bin/env bash
# B3 production-shape harness — measures the hz-probe-latency-vs-timeout
# relationship empirically instead of assuming a fixed probe time.
#
# Times the EXACT command B3 shells out (`ros2 topic hz <topic>`) at a
# given publisher rate and compares first-rate-line latency against the
# sampler's inner timeout (ROS2_SHELLOUT_TIMEOUT). Surfaces the
# 1 Hz-marginal / sub-Hz-timeout finding that drove the v1.1.3 5s→8s
# refinement (P5 DISPATCH 9A).
#
# DRAFTED — not live-validated as of v1.1.3 (no ROS2 host available
# during P5). LOCAL-RUN ONLY — see README "CI status".
#
# Dependencies: ROS2 installed + sourceable (/opt/ros/<distro>/setup.bash),
# `ros2` on PATH, `bc`.
#
# Exit: 0 = the probe's first-rate-line latency fits inside the inner
#           timeout with ≥1s margin (rate observable). 1 = missing
#           deps / latency exceeds or is marginal against the inner
#           timeout (rate NOT reliably observable — the structural
#           condition behind BUG-P5-2 for sub-Hz).
#
# Usage: RATE=1 INNER=8 ROS_SETUP=/opt/ros/humble/setup.bash ./b3_ros2_harness.sh
set -euo pipefail
RATE="${RATE:-1}"
INNER="${INNER:-8}"   # v1.1.3 ROS2_SHELLOUT_TIMEOUT (seconds)
ROS_SETUP="${ROS_SETUP:-/opt/ros/humble/setup.bash}"

fail() { echo "FAIL: $*" >&2; exit 1; }

command -v bc >/dev/null || fail "bc not installed"
[[ -f "$ROS_SETUP" ]] || fail "ROS setup not found at $ROS_SETUP (set ROS_SETUP=…)"
# ROS setup scripts reference unbound vars — relax -u across the source.
set +u
# shellcheck disable=SC1090
source "$ROS_SETUP"
set -u
command -v ros2 >/dev/null || fail "ros2 not on PATH after sourcing $ROS_SETUP"

topic="/p5_harness_${RATE/./_}"
ros2 topic pub --rate "$RATE" "$topic" std_msgs/msg/String "data: 'x'" > /dev/null 2>&1 &
pub=$!
sleep 2

tmp="/tmp/b3h_$$.txt"
start=$(date +%s.%N)
stdbuf -oL ros2 topic hz "$topic" > "$tmp" 2>&1 &
hz=$!
first=""
for _ in $(seq 1 400); do
  if grep -q "average rate" "$tmp" 2>/dev/null; then
    first=$(date +%s.%N)
    break
  fi
  sleep 0.1
done
kill "$hz" "$pub" 2>/dev/null || true
wait "$hz" "$pub" 2>/dev/null || true

if [[ -z "$first" ]]; then
  rm -f "$tmp"
  fail "rate=${RATE}Hz: no rate line within 40s — far exceeds the ${INNER}s inner timeout."
fi

lat=$(echo "$first - $start" | bc)
margin=$(echo "$INNER - $lat" | bc)
echo "rate=${RATE}Hz  first-rate-line=${lat}s  inner-timeout=${INNER}s  margin=${margin}s"
rm -f "$tmp"

if (( $(echo "$lat > $INNER" | bc) )); then
  fail "first-rate-line (${lat}s) EXCEEDS inner timeout (${INNER}s) — sampler \
kills the probe before it emits; topic unobservable at this rate (BUG-P5-2 \
territory for sub-Hz)."
fi
if (( $(echo "$margin < 1.0" | bc) )); then
  fail "MARGINAL: margin ${margin}s < 1s — under load/jitter this rate will \
intermittently time out. Consider a larger ROS2_SHELLOUT_TIMEOUT."
fi
echo "PASS: rate observable with ${margin}s margin under the ${INNER}s inner timeout."
exit 0
