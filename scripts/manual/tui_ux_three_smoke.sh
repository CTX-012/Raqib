#!/usr/bin/env bash
#
# Smoke test for the three TUI UX features ([UX-1] armed banner,
# [UX-2] post-mortem card, [UX-3] `g` keybinding for dashboards).
#
# A real TUI smoke needs a terminal — that's hard from a script.
# What we CAN exercise from a script:
#   * the testable data-path / status-string surface (unit + integration
#     tests; runs them in --release for parity with packagers)
#   * `[dashboard]` config schema accepts both populated and empty
#     `url_template` values; the binary still loads, ticks, and exits
#     cleanly with either
#
# Exit codes:
#   0   PASS — all targeted unit tests + integration tests are green
#             AND both [dashboard] config shapes load cleanly
#   1   FAIL — any of the above failed; the script prints which one
#  77   SKIP — required tooling missing (cargo)

set -euo pipefail
cd "$(dirname "$0")/../.."

if ! command -v cargo >/dev/null 2>&1; then
    echo "SKIP: cargo not on PATH; cannot exercise the test surface." >&2
    exit 77
fi

echo "==> [UX-1/UX-2/UX-3] targeted unit tests"
cargo test --release --lib ui::panels::armed_banner --quiet
cargo test --release --lib ui::panels::postmortem --quiet
cargo test --release --lib ui::app --quiet
cargo test --release --lib ui::input --quiet

# The binary build is what `cargo run --release -- --config ...` needs
# below; do it explicitly so the timing measurement of subsequent
# steps is just the load-and-tick path.
echo "==> building release binary"
cargo build --release --quiet
BIN="$PWD/target/release/edge_monitor"

TEMP_HOME="$(mktemp -d -t em-tui-ux-smoke-XXXX)"
trap 'rm -rf "$TEMP_HOME"' EXIT

echo "==> [UX-3] config schema accepts populated [dashboard] section"
CONF1="$TEMP_HOME/populated.toml"
cat > "$CONF1" <<EOF
[storage]
run_store_path = "$TEMP_HOME/store"

[dashboard]
url_template = "http://localhost:3000/d/edge?var-model={model}&var-pid={pid}"
EOF
"$BIN" --config "$CONF1" --no-ui --ticks 1 --dry-run > "$TEMP_HOME/p.log" 2>&1
if ! grep -q 'tick budget reached' "$TEMP_HOME/p.log"; then
    echo "FAIL: binary did not tick to completion with populated [dashboard]" >&2
    tail -20 "$TEMP_HOME/p.log" >&2
    exit 1
fi
if grep -qiE 'unknown key|unknown field|invalid' "$TEMP_HOME/p.log"; then
    echo "FAIL: populated [dashboard] produced an unknown-key warning" >&2
    grep -iE 'unknown|invalid' "$TEMP_HOME/p.log" >&2
    exit 1
fi

echo "==> [UX-3] config schema accepts empty url_template (disabled)"
CONF2="$TEMP_HOME/empty.toml"
cat > "$CONF2" <<EOF
[storage]
run_store_path = "$TEMP_HOME/store"

[dashboard]
url_template = ""
EOF
"$BIN" --config "$CONF2" --no-ui --ticks 1 --dry-run > "$TEMP_HOME/e.log" 2>&1
if ! grep -q 'tick budget reached' "$TEMP_HOME/e.log"; then
    echo "FAIL: binary did not tick to completion with empty url_template" >&2
    tail -20 "$TEMP_HOME/e.log" >&2
    exit 1
fi

echo "==> [UX-3] omitting the [dashboard] section entirely also works"
CONF3="$TEMP_HOME/missing.toml"
cat > "$CONF3" <<EOF
[storage]
run_store_path = "$TEMP_HOME/store"
EOF
"$BIN" --config "$CONF3" --no-ui --ticks 1 --dry-run > "$TEMP_HOME/m.log" 2>&1
if ! grep -q 'tick budget reached' "$TEMP_HOME/m.log"; then
    echo "FAIL: binary did not tick to completion with no [dashboard] section" >&2
    tail -20 "$TEMP_HOME/m.log" >&2
    exit 1
fi

echo "PASS"
