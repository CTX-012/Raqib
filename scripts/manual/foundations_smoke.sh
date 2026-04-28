#!/usr/bin/env bash
#
# Smoke test for latest.md Foundation A (RunStore) + Foundation C
# (analysis::compare). These have no user-visible CLI surface yet —
# Tier 1.1 will expose `edge_monitor history` — so this script just
# exercises the unit-test suite for both modules and prints pass/fail.
#
# Re-run from the repo root or from anywhere; the cd line normalises.
#
# Exit codes:
#   0  all tests passed
#   1  build or test failure

set -euo pipefail
cd "$(dirname "$0")/../.."

echo "==> Foundation A (storage::run_store)"
cargo test --lib --quiet storage::run_store
echo
echo "==> Foundation C (analysis::compare)"
cargo test --lib --quiet analysis::compare
echo
echo "==> ALL OK"
