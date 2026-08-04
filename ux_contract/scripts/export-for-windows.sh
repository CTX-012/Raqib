#!/usr/bin/env bash
set -euo pipefail
OUT="${1:-/tmp/ux_contract_export}"
rm -rf "$OUT"
mkdir -p "$OUT/src" "$OUT/tests" 2>/dev/null || true
cp Cargo.toml "$OUT/"
# Copy every .rs source. CAR-17 (v0.3.8) introduced the multi-file
# pattern by adding `src/kill_confirm_card.rs` as `pub mod
# kill_confirm_card;` from lib.rs. Earlier versions had only lib.rs, so
# the script previously hardcoded `cp src/lib.rs` — that silently
# dropped submodule files from the export and produced bundles that
# fail to compile downstream (unresolved module). Glob so future
# submodule files are picked up automatically.
cp src/*.rs "$OUT/src/"
if [ -d tests ]; then cp -r tests/* "$OUT/tests/" 2>/dev/null || true; fi
( cd "$OUT" && find . -type f \( -name '*.rs' -o -name 'Cargo.toml' \) \
    -exec sha256sum {} \; ) | sort > "$OUT/SHA256SUMS"
echo "Exported to $OUT"
echo "Contents:"
( cd "$OUT" && find . -type f | sort )
echo
echo "SHA256SUMS:"
cat "$OUT/SHA256SUMS"
