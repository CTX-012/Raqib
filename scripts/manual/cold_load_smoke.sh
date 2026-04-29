#!/usr/bin/env bash
#
# Smoke test for latest.md Tier 2.2 — cold-load disk I/O detection.
#
# Verifies: when an AI-classified process reads ≥16 MiB from disk
# during startup and then plateaus, `RunRecord.cold_start` is
# populated with a non-null `duration_seconds` and `bytes_read`.
#
# Method: write a 256 MiB synthetic .gguf in a sibling subshell with
# `dd ... oflag=direct` to avoid the page cache, then launch a python
# process whose argv contains `--model <gguf>` (so the classifier
# picks it up as AI Inference). The process posix_fadvise()s the
# file out of cache, then reads it end-to-end, then idles for ≥2 s
# so the cold-load detector observes the plateau. Then it exits so
# the runtime stamps `cold_start` onto its `RunRecord`.
#
# WSL caveat: Microsoft's ext4-on-VHD kernel populates /proc/<pid>/io
# `read_bytes` only for storage-device hits, not page-cache hits, AND
# its writeback aggregates writes into the cache aggressively so even
# `oflag=direct` may not always evict. If we run two reads and the
# second still doesn't move read_bytes, we treat the smoke test as
# SKIP rather than FAIL (the unit tests in src/telemetry/cold_load.rs
# already cover the tracker's logic deterministically; this manual
# script's job is the integration path through the runtime, which we
# cannot exercise honestly when the kernel won't expose the byte
# counter).
#
# Exit codes:
#   0   PASS — RunRecord.cold_start.duration_seconds populated.
#   1   FAIL — read_bytes moved AND ≥16 MiB AND plateau, yet
#             cold_start was still None — that's a runtime bug.
#   77  SKIP — Python 3 unavailable, /proc/<pid>/io unreadable, OR
#             the host kernel doesn't surface read_bytes for our
#             process (e.g. WSL with everything cached in RAM).

set -euo pipefail
cd "$(dirname "$0")/../.."

PYTHON="${PYTHON:-python3}"
if ! command -v "$PYTHON" >/dev/null 2>&1; then
    echo "SKIP: requires python3 on PATH." >&2
    exit 77
fi
if [[ ! -r "/proc/self/io" ]]; then
    echo "SKIP: /proc/<pid>/io is not readable on this kernel; Tier 2.2" >&2
    echo "      cannot observe disk reads without it." >&2
    exit 77
fi

TEMP_HOME="$(mktemp -d -t em-cold-load-smoke-XXXX)"
trap 'rm -rf "$TEMP_HOME"; jobs -p | xargs -r kill -9 2>/dev/null || true' EXIT

echo "==> building release binary"
cargo build --release --quiet
BIN="$PWD/target/release/edge_monitor"

CONF="$TEMP_HOME/edge_monitor.toml"
cat > "$CONF" <<EOF
[runtime]
tick_interval_ms = 500

[storage]
run_store_path = "$TEMP_HOME/run_store"
EOF

WEIGHTS="$TEMP_HOME/fake.gguf"
echo "==> generating 256 MiB synthetic .gguf via dd oflag=direct"
# oflag=direct bypasses the page cache so the bytes hit storage and
# can be re-read from there (which is what populates read_bytes).
if ! dd if=/dev/urandom of="$WEIGHTS" bs=1M count=256 \
    oflag=direct status=none 2>/dev/null; then
    # Some filesystems / WSL versions reject O_DIRECT on small
    # tmpdirs. Fall back to a normal write + sync; we'll detect the
    # cache problem below.
    dd if=/dev/urandom of="$WEIGHTS" bs=1M count=256 status=none
    sync
fi

# Pre-flight check: can our test PID see read_bytes change at all on
# this kernel? If not, SKIP (the unit tests cover the tracker logic).
PROBE_OUT="$("$PYTHON" - <<EOF
import os
fd = os.open('$WEIGHTS', os.O_RDONLY)
try:
    os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
except Exception:
    pass
buf = bytearray(1<<20)
mv = memoryview(buf)
# Read a slab to bump read_bytes
for _ in range(64):
    n = os.readv(fd, [mv])
    if n == 0: break
os.close(fd)
io = open(f'/proc/{os.getpid()}/io').read().splitlines()
rb = next(int(l.split()[1]) for l in io if l.startswith('read_bytes:'))
print(rb)
EOF
)"
if [[ -z "$PROBE_OUT" ]] || [[ "$PROBE_OUT" -lt $((16 * 1024 * 1024)) ]]; then
    echo "SKIP: this kernel only reported read_bytes=$PROBE_OUT after a" >&2
    echo "      64 MiB read with posix_fadvise(POSIX_FADV_DONTNEED). The" >&2
    echo "      cold-load detector cannot observe a burst from cache hits;" >&2
    echo "      run on a non-WSL host or one whose /proc accounting" >&2
    echo "      surfaces storage-device reads. Tracker unit tests in" >&2
    echo "      src/telemetry/cold_load.rs cover the logic deterministically." >&2
    exit 77
fi
echo "==> /proc/io probe shows read_bytes=$PROBE_OUT — proceeding"

WORKLOAD="$TEMP_HOME/loader.py"
cat > "$WORKLOAD" <<'EOF'
import os, sys, time
# Argv: --model <path>. Two phases so the cold-load detector
# observes the burst-then-plateau shape:
#   1. Sleep 1.5 s — gives edge_monitor's first tick a chance to
#      sample /proc/<pid>/io with read_bytes still at 0, so the
#      initial state is anchored before reads begin. Otherwise the
#      tracker's first observation captures the post-burst counter
#      and computes delta=0 forever.
#   2. Drop page cache via posix_fadvise(POSIX_FADV_DONTNEED), then
#      read the whole file from disk. /proc/<pid>/io counts only
#      storage-device bytes — page-cache hits don't bump read_bytes
#      — so the fadvise hint is what makes the burst visible.
#   3. Idle 4 s for the plateau detector's ≥2 consecutive ≤1 MiB/s
#      ticks to fire.
path = None
i = 1
while i < len(sys.argv):
    if sys.argv[i] == '--model' and i + 1 < len(sys.argv):
        path = sys.argv[i+1]
        break
    i += 1
assert path, 'no --model'
time.sleep(1.5)
fd = os.open(path, os.O_RDONLY)
try:
    os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
except (AttributeError, OSError):
    pass  # rare older platforms — best effort
total = 0
chunk = 1 << 20
while True:
    buf = os.read(fd, chunk)
    if not buf:
        break
    total += len(buf)
os.close(fd)
print(f'read={total}', flush=True)
time.sleep(4)
EOF

echo "==> launching loader (background) and edge_monitor (foreground)"
"$PYTHON" "$WORKLOAD" --model "$WEIGHTS" > "$TEMP_HOME/loader.log" 2>&1 &
LOADER_PID=$!

# 18 ticks of 500 ms = 9 s. Loader budget: 1.5 s pre-sleep + ~0.5 s
# burst + 4 s plateau = 6 s, well inside the 9 s window so
# edge_monitor outlives the loader and writes its RunRecord.
"$BIN" --config "$CONF" --no-ui --ticks 18 --dry-run \
    > "$TEMP_HOME/em.log" 2>&1 || {
        echo "FAIL: edge_monitor exited non-zero" >&2
        tail -20 "$TEMP_HOME/em.log" >&2
        exit 1
    }

wait "$LOADER_PID" 2>/dev/null || true

"$BIN" --config "$CONF" history --json > "$TEMP_HOME/h-summary.json"

# Find the per-model record set and look for any with cold_start set.
if "$PYTHON" -c "
import json, subprocess, sys
summary = json.load(open('$TEMP_HOME/h-summary.json'))
for d in summary:
    name = d.get('model') or ''
    if not name:
        continue
    out = subprocess.check_output(['$BIN','--config','$CONF','history',name,'--json'])
    runs = json.loads(out)
    for r in runs:
        cs = r.get('cold_start')
        if cs and cs.get('duration_seconds') is not None \
            and cs.get('bytes_read', 0) >= 16 * 1024 * 1024:
            print(f'cold_start={cs}')
            sys.exit(0)
        if cs:
            print(f'cold_start present but unexpected shape: {cs}',
                  file=sys.stderr)
sys.exit(1)
"; then
    echo "PASS"
    exit 0
else
    echo "FAIL: no RunRecord had cold_start populated with ≥16 MiB read" >&2
    "$PYTHON" -m json.tool < "$TEMP_HOME/h-summary.json" >&2 || true
    echo "FAIL"
    exit 1
fi
