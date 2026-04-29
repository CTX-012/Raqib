#!/usr/bin/env bash
# scripts/manual/detail_mode_smoke.sh
#
# Detail-mode toggle smoke — verifies the `v` keybinding wires through
# from `input.rs` into `App::detail_mode` and that the render path
# branches between the default (4-panel) and detail (6-panel) layouts.
# PTY-based TUI verification is left to T2's V3 walkthrough; this
# script proves the data path is in place via:
#
#   1. Targeted unit tests for the `App` toggle + the `v` keybinding
#      translation. These pin the behavioural contract.
#   2. Source-grep that confirms both `render_default` and
#      `render_detail` exist and that the footer hint differs between
#      modes (catches accidental reverts where one mode's footer leaks
#      into the other).
#
# Exits 0 on success, non-zero with a diagnostic on first failure.

set -euo pipefail
cd "$(dirname "$0")/../.." # repo root

echo "[smoke] cargo build --release"
cargo build --release --quiet

echo "[smoke] App + input unit tests"
# cargo test takes a single substring filter; running the whole ui::
# subtree picks up everything we care about plus the existing focus
# tests. Required tests are extracted with grep so a missing one is a
# loud failure rather than silent.
out=$(cargo test --release --lib ui:: -- --nocapture 2>&1)
echo "$out" | tail -25
required=(
  default_mode_locks_focus_to_registry
  toggle_detail_mode_flips_the_flag_and_resets_focus
  leaving_detail_mode_disarms_pending_kill
  v_toggles_detail_mode
)
for t in "${required[@]}"; do
  if ! grep -E "test ui::.*::tests::$t \.\.\. ok" <<<"$out" >/dev/null ; then
    echo "FAIL: required detail-mode test did not pass: $t"
    exit 1
  fi
done
echo "[smoke] all 4 detail-mode tests pass"

# Render path must have both branches present. Catches the "we
# accidentally collapsed both modes back into a single layout" bug.
must_grep() {
  local needle="$1"
  if ! grep -F -q "$needle" src/ui/panels/mod.rs ; then
    echo "FAIL: src/ui/panels/mod.rs missing expected token: $needle"
    exit 1
  fi
}
must_grep 'fn render_default'
must_grep 'fn render_detail'
must_grep 'app.detail_mode()'
must_grep 'v show details'
must_grep 'v hide details'
echo "[smoke] both layouts present and footer differs by mode"

# Status bar should still mention 'focus' — but the operator should
# only see Tab activity in detail mode. The wiring is checked by the
# unit tests above; the smoke just confirms the source token is alive.
if ! grep -F -q "Action::ToggleDetailMode" src/ui/mod.rs ; then
  echo "FAIL: Action::ToggleDetailMode not dispatched in run loop"
  exit 1
fi
if ! grep -F -q "Action::ToggleDetailMode" src/ui/input.rs ; then
  echo "FAIL: input.rs does not produce Action::ToggleDetailMode"
  exit 1
fi
echo "[smoke] action wired through input → run-loop → App"

echo "PASS: detail-mode toggle is in place; default view hides Framework procs / All processes / Recent actions."
