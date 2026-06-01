#!/usr/bin/env bash
# B3 production-shape harness — v1.1.6 ITEM 2.
#
# Mirrors the EXACT subprocess shape B3's `observe_topic_echo` invokes:
#   ros2 topic echo --once <topic>
#
# v1.1.5 shipped with `--once --timeout <T>` which Humble's
# ros-humble-ros2cli 0.18.18 REJECTS (`unrecognized arguments:
# --timeout` — the flag was added in Iron/Jazzy/Rolling). Every
# probe failed; every ROS2 row locked to Idle. The pre-v1.1.6 harness
# tested `ros2 topic hz` and silently passed against the broken
# echo-once shape — the harness-drift gap this harness exists to
# prevent. v1.1.6 ITEM 1 dropped the flag; ITEM 2 (this rewrite) keeps
# the harness honest by:
#   1. spawning the EXACT B3 invocation against a live publisher and
#      measuring first-message latency vs ROS2_SHELLOUT_TIMEOUT;
#   2. asserting `--once --timeout 1` STILL fails on this host, so
#      any future re-introduction of the flag trips the harness on
#      Humble before it ships.
#
# DRAFTED — not live-CI-validated (LOCAL-RUN ONLY per README "CI
# status"). Sampler unit-test coverage lives in
# `src/telemetry/samplers/ros2_shellout.rs` —
# `b3_echo_once_no_timeout_flag_detects_active_topic` pins the args
# list shape from the Rust side.
#
# Dependencies: ROS2 installed + sourceable
# (/opt/ros/<distro>/setup.bash), `ros2` on PATH, `bc`.
#
# Exit: 0 = echo-once observes a message inside the inner timeout
#           with ≥ 0.5s margin AND `--timeout` is rejected
#           (Humble-compat guard holds).
#       1 = missing deps / echo-once exceeds inner timeout / Humble's
#           `--timeout` rejection regressed (someone made the flag
#           supported, which would invalidate ITEM 1's reasoning).
#
# Usage:
#   RATE=1 INNER=3 ROS_SETUP=/opt/ros/humble/setup.bash \
#     ./b3_ros2_harness.sh
set -euo pipefail
RATE="${RATE:-1}"
INNER="${INNER:-3}"   # v1.1.6 ROS2_SHELLOUT_TIMEOUT (seconds)
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

# ─── Step A: Humble-compat guard (v1.1.6 ITEM 1) ────────────────────
# Confirm `--timeout` is STILL rejected on this host. If a future ros2cli
# point release backports the flag to Humble, the unit-test regression
# pin would still hold (we'd be merely "ignoring a now-supported flag")
# but this harness flags the change so the dispatch can re-evaluate
# whether to add it back.
guard_topic="/p5_harness_guard"
ros2 topic pub --rate "$RATE" "$guard_topic" std_msgs/msg/String "data: 'x'" > /dev/null 2>&1 &
guard_pub=$!
sleep 2
guard_out="/tmp/b3h_guard_$$.txt"
set +e
ros2 topic echo --once --timeout 1 "$guard_topic" > "$guard_out" 2>&1
guard_rc=$?
set -e
kill "$guard_pub" 2>/dev/null || true
wait "$guard_pub" 2>/dev/null || true
if [[ $guard_rc -eq 0 ]]; then
  cat "$guard_out" >&2
  rm -f "$guard_out"
  fail "Humble-compat guard regressed: \`ros2 topic echo --once --timeout 1\` \
SUCCEEDED on this host. v1.1.6 ITEM 1 dropped --timeout because Humble's \
ros-humble-ros2cli 0.18.18 REJECTED it. If --timeout is now supported, \
re-evaluate the dispatch (the regression-pin unit test still holds, but \
the rationale changed)."
fi
if ! grep -q -- "--timeout" "$guard_out"; then
  cat "$guard_out" >&2
  rm -f "$guard_out"
  fail "Humble-compat guard returned non-zero ($guard_rc) but stderr did NOT \
mention --timeout — different failure mode than v1.1.5 hit. Investigate."
fi
rm -f "$guard_out"
echo "GUARD OK: Humble ros2cli rejects \`--timeout\` on \`topic echo\` (rc=$guard_rc)."

# ─── Step B: measure echo-once first-message latency ────────────────
topic="/p5_harness_${RATE/./_}"
ros2 topic pub --rate "$RATE" "$topic" std_msgs/msg/String "data: 'x'" > /dev/null 2>&1 &
pub=$!
sleep 2

tmp="/tmp/b3h_echo_$$.txt"
start=$(date +%s.%N)
# EXACT B3 invocation shape (src/telemetry/samplers/ros2_shellout.rs
# ::ros2_echo_args). No --timeout. --once self-terminates on the first
# message; this echo process should exit on its own well inside INNER.
( ros2 topic echo --once "$topic" > "$tmp" 2>&1 ) &
echo_pid=$!

# Poll for non-empty stdout content (the same arrival signal
# `stdout_observed_message` uses inside B3).
first=""
for _ in $(seq 1 $((INNER * 20))); do
  if [[ -s "$tmp" ]] && grep -q -v '^$' "$tmp" 2>/dev/null; then
    first=$(date +%s.%N)
    break
  fi
  sleep 0.05
done

# echo --once should self-terminate after the first message — give it a
# moment, then force-kill if it lingered (matches B3's belt-and-braces
# kill_on_drop + outer timeout kill).
wait_count=0
while kill -0 "$echo_pid" 2>/dev/null && (( wait_count < 20 )); do
  sleep 0.1
  # `((wait_count++))` would return 0 the first iteration and trip
  # `set -e`; use the arithmetic-assignment form which always
  # returns success.
  wait_count=$((wait_count + 1))
done
kill "$echo_pid" 2>/dev/null || true
kill "$pub" 2>/dev/null || true
wait "$echo_pid" "$pub" 2>/dev/null || true

if [[ -z "$first" ]]; then
  echo "--- echo stdout/stderr ---" >&2
  cat "$tmp" >&2 || true
  rm -f "$tmp"
  fail "rate=${RATE}Hz: no message observed within ${INNER}s — \
echo-once mechanism failed against a live publisher. This is the \
v1.1.5 BUG-P5-2 shape (every topic locks to Idle)."
fi

lat=$(echo "$first - $start" | bc)
margin=$(echo "$INNER - $lat" | bc)
echo "rate=${RATE}Hz  first-message=${lat}s  inner-timeout=${INNER}s  margin=${margin}s"
rm -f "$tmp"

if (( $(echo "$lat > $INNER" | bc) )); then
  fail "first-message (${lat}s) EXCEEDS inner timeout (${INNER}s) — B3 \
would cancel the probe before the message arrives; topic unobservable \
at this rate."
fi
if (( $(echo "$margin < 0.5" | bc) )); then
  fail "MARGINAL: margin ${margin}s < 0.5s — under load/jitter this rate \
will intermittently time out. Consider raising ROS2_SHELLOUT_TIMEOUT."
fi
echo "PASS: echo-once observes a message in ${lat}s with ${margin}s \
margin under the ${INNER}s inner timeout."
exit 0
