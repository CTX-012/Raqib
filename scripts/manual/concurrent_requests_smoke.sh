#!/usr/bin/env bash
# scripts/manual/concurrent_requests_smoke.sh
#
# Tier 3.4 smoke — exercises the concurrent-request awareness data
# path on the local box and prints human-readable evidence:
#
#   1. Targeted unit tests for `telemetry::concurrent_requests`:
#      verifies the time-weighted gauge math (textbook integral on
#      "1 req for 10s, 8 for 50s" ≈ 6.833) and the boundary cases
#      (single sample, all zeros, backwards-time).
#
#   2. End-to-end integration test `tests/concurrent_requests_e2e.rs`:
#      drives synthetic frames through the accumulator and asserts
#      `RunMetrics::concurrent_requests_{avg,peak,waiting_peak}` are
#      populated correctly.
#
#   3. vLLM sampler test that exercises `vllm:num_requests_waiting`
#      parsing (Tier 3.4's saturation-signal source).
#
# Skips the live-server smoke deliberately — `scripts/manual/vllm_smoke.sh`
# already exercises a real vLLM end to end and would just duplicate
# infrastructure here. This script is the "did the pure-logic Tier 3.4
# feature land?" gate.
#
# Exits 0 on success, non-zero with a tag on the first failure.

set -euo pipefail

cd "$(dirname "$0")/../.." # repo root

echo "[smoke] cargo build --release"
cargo build --release --quiet

echo "[smoke] running targeted gauge unit tests"
cargo test --release --lib telemetry::concurrent_requests -- --nocapture | tail -25

echo "[smoke] running Tier 3.4 integration test"
cargo test --release --test concurrent_requests_e2e -- --nocapture | tail -15

echo "[smoke] verifying vllm:num_requests_waiting plumbing"
cargo test --release --lib telemetry::samplers::vllm_prometheus::tests::frame_from_metrics_maps_vllm_names \
  -- --nocapture | tail -10

# Final independent calculation in shell of the spec's textbook
# integral, displayed alongside the assertion in the test, so an
# operator reading the smoke output sees the expected value
# computed two ways.
echo
python3 - <<'PY'
runs = [(1, 10), (8, 50)]   # (concurrency, seconds)
total_value_seconds = sum(c * s for c, s in runs)
total_seconds       = sum(s for _, s in runs)
avg = total_value_seconds / total_seconds
print(f"[smoke] textbook time-weighted avg for {runs}: {avg:.3f}")
print("[smoke] tests assert this matches the gauge's average() output")
PY

echo "PASS: Tier 3.4 concurrent-request gauge + accumulator + vLLM parsing all green."
