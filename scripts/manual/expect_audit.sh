#!/usr/bin/env bash
# scripts/manual/expect_audit.sh
#
# S.3 guard — every `expect(` call in src/ outside `#[cfg(test)]` must be
# preceded (within 8 lines) by an `// ok: expect — <reason>` comment.
# CLAUDE.md documents the three accepted invariants.
#
# Exits 0 when every PROD expect() site is annotated, non-zero with a
# list of offending sites otherwise.

set -euo pipefail

cd "$(dirname "$0")/../.." # repo root

missing=0
report=$(mktemp)
trap 'rm -f "$report"' EXIT

# Prefer rg if present; otherwise grep -rl. We need every .rs under src/
# that contains the literal "expect(".
if command -v rg >/dev/null 2>&1; then
  files=$(rg -l 'expect\(' src/ || true)
else
  files=$(grep -rl --include='*.rs' 'expect(' src/ || true)
fi
if [[ -z "$files" ]]; then
  echo "no expect() calls in src/ — nothing to audit"
  exit 0
fi

for f in $files; do
  awk -v file="$f" '
    /^#\[cfg\(test\)\]/ { in_test = 1 }
    /expect\(/ {
      if (!in_test) {
        ok = 0
        for (i = NR - 1; i >= NR - 8 && i > 0; i--) {
          if (history[i] ~ /ok: expect/) { ok = 1; break }
        }
        if (!ok) {
          printf "%s:%d  %s\n", file, NR, $0
        }
      }
    }
    { history[NR] = $0 }
  ' "$f" >> "$report"
done

if [[ -s "$report" ]]; then
  echo "FAIL: unannotated expect() sites in non-test code:"
  cat "$report"
  exit 1
fi

if command -v rg >/dev/null 2>&1; then
  count=$(rg -n 'expect\(' src/ | wc -l)
else
  count=$(grep -rn --include='*.rs' 'expect(' src/ | wc -l)
fi
echo "PASS: $count expect() sites scanned (annotated PROD or test); none violate the rule."
