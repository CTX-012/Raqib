#!/usr/bin/env bash
# scripts/manual/ux_rename_smoke.sh
#
# UX-rename pass — verifies the operator-facing label changes the user
# requested in this session. Most of the surface is TUI-side and lives
# in `src/ui/panels/*.rs`; the smoke does NOT spin up a TUI (PTY
# automation in shell is fragile). Instead it:
#
#   1. Runs the dry-run-format unit test that pins the new
#      `Would stop <name> (dry-run mode — no action taken)` shape.
#   2. Greps the source tree to confirm the old labels are gone and
#      the new labels are present. Source-grep is the cheapest reliable
#      regression catch — an accidental revert in `panel_block(...)`
#      would surface here without needing to open a terminal emulator.
#   3. Runs `edge_monitor --no-ui --ticks 2` headless and asserts the
#      stderr does NOT contain `model=-`, `vram=0M`, or `AI-classified:`
#      — the headless renderer also got cleaned up in the same pass.
#
# Exits 0 on success, non-zero with a diagnostic on the first failure.

set -euo pipefail
cd "$(dirname "$0")/../.." # repo root

echo "[smoke] cargo build --release"
cargo build --release --quiet

echo "[smoke] dry-run format unit test"
cargo test --release --lib \
  governor::executor::tests::dry_run_reason_string_uses_process_name_and_plain_english \
  -- --nocapture | tail -5

# Old strings that must no longer appear in source (panel titles + the
# Vitals "AI-classified" / "NVML uninitialized" lines + the shouty
# DRY-RUN prefix). The source-grep is more robust than running the
# binary because it catches the issue at edit-time.
old_strings=(
  'Registry (AI workloads)'
  'Rogues (unmapped framework procs)'
  'Culprits (top by PID order)'
  'Audit (kills + regressions)'
  'AI run summaries'
  'NVML uninitialized'
  'AI-classified:'
  'DRY-RUN: would send'
)
for s in "${old_strings[@]}"; do
  if grep -rn --include='*.rs' -F "$s" src/ ; then
    echo "FAIL: old user-facing string still present: \"$s\""
    exit 1
  fi
done
echo "[smoke] all old labels gone from src/"

# New strings that should now be present.
new_strings=(
  'AI Workloads'
  'Framework procs'
  'All processes'
  'Recent actions'
  'Recent runs'
  'No GPU detected'
  'AI workloads detected'
  'Would stop'
)
missing=0
for s in "${new_strings[@]}"; do
  if ! grep -rn --include='*.rs' -F "$s" src/ >/dev/null ; then
    echo "FAIL: expected new string not found in source: \"$s\""
    missing=1
  fi
done
(( missing == 0 )) || exit 1
echo "[smoke] all new labels present in src/"

# Headless run: model=- / vram=0M / AI-classified: must not surface.
log=$(mktemp)
trap 'rm -f "$log"' EXIT
./target/release/edge_monitor --no-ui --ticks 2 2>"$log" >/dev/null
if grep -E 'model=-|vram=0M|AI-classified:|DRY-RUN: would send' "$log" ; then
  echo "FAIL: headless stderr still contains an old user-facing string"
  exit 1
fi
echo "[smoke] headless stderr is clean of the old placeholders"

echo "PASS: UX rename pass landed end-to-end."
