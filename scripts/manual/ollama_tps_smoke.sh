#!/usr/bin/env bash
# scripts/manual/ollama_tps_smoke.sh
#
# Tester 2 V1 follow-up — verifies that `stdout_parser.rs` now
# extracts Ollama's `eval rate: N tokens/s` line as a tokens/sec
# metric, and that the existing vLLM / llama.cpp parsers do not
# regress. Distinct from `ollama_smoke.sh` which exercises the
# /api/ps model-name path (Tier 1.2c) — this one targets the
# tokens/sec ground-truth path that V1 found broken.
#
# Modes:
#   1. Default: runs the targeted unit tests (always works).
#   2. With OLLAMA_FIXTURE_DIR=/path/to/dir holding *.out files
#      from `ollama run --verbose`, also greps each fixture for
#      `eval rate:` and asserts at least one match — proves the
#      regex covers the operator's actual Ollama version.
#
# T2 captured trial files live at /tmp/v1_trial_{1,2,3}.out on
# the test rig; if those exist locally, they are used by default.
#
# Exits 0 on success, non-zero with a diagnostic on first failure.

set -euo pipefail
cd "$(dirname "$0")/../.." # repo root

echo "[smoke] cargo build --release"
cargo build --release --quiet

echo "[smoke] running stdout_parser tests (Ollama + cross-runtime regression)"
cargo test --release --lib telemetry::samplers::stdout_parser \
  -- --nocapture | tail -20

# Locate fixtures: explicit env var beats T2's well-known /tmp paths.
fixture_dir="${OLLAMA_FIXTURE_DIR:-/tmp}"
matched=0
mapfile -t candidates < <(ls "$fixture_dir"/v1_trial_*.out 2>/dev/null || true)

if (( ${#candidates[@]} > 0 )); then
  echo "[smoke] checking ${#candidates[@]} captured Ollama trial fixtures"
  for f in "${candidates[@]}"; do
    line=$(grep -E '^[[:space:]]*eval rate:[[:space:]]+[0-9]+(\.[0-9]+)?[[:space:]]+tokens?/s' "$f" \
            | head -1 || true)
    if [[ -z "$line" ]]; then
      echo "  WARN: $f has no `eval rate:` line — Ollama version drift?"
      continue
    fi
    echo "  $f → $line"
    matched=$((matched + 1))
  done

  if (( matched == 0 )); then
    echo "FAIL: no fixture had a parseable `eval rate:` line"
    exit 1
  fi
else
  echo "[smoke] no /tmp/v1_trial_*.out fixtures found — skipping live-fixture check"
  echo "[smoke] re-run with OLLAMA_FIXTURE_DIR=<dir> after `ollama run --verbose ...`"
fi

echo "PASS: stdout_parser handles Ollama eval rate; vLLM/llama.cpp untouched."
