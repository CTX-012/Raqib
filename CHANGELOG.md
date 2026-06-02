# Changelog

All notable changes to `edge_monitor` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project aims to adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once `v1.0.0` is tagged. Until then, minor versions may include breaking changes.

## [1.1.9] — 2026-06-01 — B3 spawn-churn fix (cadence + backoff + global cache)

Closes the v1.1.8 STOP-AND-SURFACE filing on the ~166 MB/min
residual leak. DISPATCH 28 operator strace identified the
ground-truth root cause: 440,409 close() calls in 70 s
(~6.3 K/s, 99.96 % of all syscalls), dominated by python
interpreter teardown from B3's per-tick `ros2 topic echo --once`
subprocess spawns. The v1.1.8 eventfd-leak hypothesis is
retired in favour of this ground-truth diagnosis.

Three operator-locked fixes shipped (one commit each):

  - ITEM (c) **cadence-gate echo probes at 10 s** (~82 % of spawns).
  - ITEM (b) **topic-list failure backoff** via attempt/success
    timestamp separation (~18 % failure amplifier).
  - ITEM (a) **sampler-global topic-list cache** (Jetson flat-RSS
    standard; the residual per-PID redundancy).

Expected close()-rate post-fix: ~6,290/s → ~665/s (Inspector
estimate). The empirical close()-rate validation runs operator-
side per DISPATCH 30; v1.1.9 unit-test gates do NOT block on it.

### Fixed

- **B3 topic-list failure backoff (ITEM b).** Pre-v1.1.9
  `PerPidState` carried a single `last_topic_list_at` timestamp
  updated only on `Ok` from `run_topic_list`. The refresh-gate
  predicate keyed off the same field, so a transient
  `Err(Transient)` spawned a fresh `ros2 topic list` subprocess
  on EVERY subsequent tick until success — the failure
  amplifier DISPATCH 28 strace called out (~18 % of B3's spawn
  churn under flaky-graph conditions). v1.1.9 splits the field
  into `last_topic_list_success_at` (updated on `Ok` only) and
  `last_topic_list_attempt_at` (updated on every attempt before
  the await). The cadence predicate now gates on
  `last_topic_list_attempt_at`, so a failed attempt costs at
  most one spawn per `ROS2_TOPIC_LIST_INTERVAL` (30 s). Cache
  GC sweep re-anchored on the new `PerPidState.last_seen_at`
  (set every tick this sampler runs against this PID), per
  ITEM (a)'s lift. Operator-locked style was the 10-LoC
  separation (over the 1-LoC "update both on every attempt"
  shortcut) for independently readable "tried recently" vs.
  "succeeded recently" semantics.

### Changed

- **B3 echo probes are cadence-gated at 10 s (ITEM c, the big win).**
  Pre-v1.1.9 B3 spawned a fresh `ros2 topic echo --once <topic>`
  subprocess per AI ROS2 PID per tick (~1 Hz). Each spawn is a
  python interpreter + rclpy import that closes ~500 FDs on
  startup → ~5 K close()/s from just B3 under the empirical
  10-publisher workload. The cadence matched the tick rate, NOT
  any property the use case actually needs: echo-once is a
  **liveness probe** for the activity-state display, not a rate
  measurement. v1.1.9 adds `ROS2_ECHO_PROBE_INTERVAL = 10s`
  + a per-PID-per-topic `last_echo_probe_at` HashMap; within
  the cadence window B3 reuses the cached `last_message_at` +
  the staleness check (identical Active/Idle decision a fresh
  `Ok(false)` probe would produce, just without the subprocess).
  Pinned invariant:
  `ROS2_ECHO_PROBE_INTERVAL * 2 <= ROS2_ACTIVITY_STALENESS`,
  so ≥ 2 probes always fit inside the staleness window. Detection
  latency for a topic going silent: still ~30 s (same as v1.1.8),
  the cadence gate does NOT change steady-state Active/Idle
  decisions — it only stops the per-tick churn.
- **Sampler-global B3 topic-list cache (ITEM a; Jetson flat-RSS).**
  Pre-v1.1.9 each PerPidState independently cached the same
  global topic list. Under the empirical workload (10
  publishers, all first-seen at roughly the same tick), the
  per-PID 30 s cadences aligned and 10 redundant
  `ros2 topic list` spawns fired per cadence window. v1.1.9
  lifts `topic_list` + `last_topic_list_{success,attempt}_at`
  from `PerPidState` to `Ros2ShelloutSource` — one refresh per
  `ROS2_TOPIC_LIST_INTERVAL` regardless of PID count. `PerPidState`
  gains a `last_seen_at` GC anchor (updated every tick this
  sampler runs against the PID). Scope: B3-internal only, single
  file (`src/telemetry/samplers/ros2_shellout.rs`), trait surface
  unchanged. Why it matters: Jetson Orin Nano 8 GB has ~6.3 GB
  effective RAM and already OOMs on quantized models; a monitor
  must be a negligible observer — flat RSS is the real standard,
  not "10× better but still leaking."

### Notes

- All three commits are independently bisectable (ITEM c
  `2df8317`, ITEM b `0245916`, ITEM a `734e22f`).
- close()-rate validation: NOT done in this dispatch (per
  DISPATCH 29 — operator runs strace -c on v1.1.9 in DISPATCH
  30). The empirical proof of fix is the strace number, NOT the
  unit tests. Unit tests pin structural invariants only.
- Timing interaction (`ROS2_ECHO_PROBE_INTERVAL` 10 s vs
  `ROS2_ACTIVITY_STALENESS` 30 s): three probes per staleness
  window. Sub-Hz topic detection latency unchanged from v1.1.8.
- Test counts: 905 → 910 (+1 cadence-const pin, +1 cadence-
  behaviour pin, +1 cross-traffic pin, +1 backoff pin, +2 ITEM
  (a) structural pins — global-cache + PerPidState-minimal).

## [1.1.8] — 2026-06-01 — partial residual leak fix + STOP-AND-SURFACE

DISPATCH 25 follow-up on the v1.1.7 STOP-AND-SURFACE filing
(~190 MB/min residual). Inspector (DISPATCH 24) ranked candidate
(4) "tokio `Child` handle leak from `kill()` without `wait()`"
as the strongest hypothesis; PHASE 0 /proc diagnostic (heaptrack
turned out blind to mimalloc on v1.1.7 — see "Process notes"
below) observed 226 `anon_inode:[eventfd]` FDs at t=185s under
the 10× ROS2-publisher workload, growing ~1 FD/sec, consistent
with the candidate.

The two specified fixes shipped. Endurance after both fixes:

  v1.1.7 (Arc + mimalloc):    ~190 MB/min
  v1.1.8 (ITEM 1 + ITEM 2):   ~166 MB/min   (~13% improvement)
  Inspector target:           < 50 MB/min   (NOT MET)

Per DISPATCH 25 STOP-AND-SURFACE trigger 4 (residual >100 MB/min),
the residual is filed for a follow-up dispatch with a concrete
diagnostic-tools list. Full empirical artefacts at
`tests/empirical/v1_1_8/`.

### Fixed

- **B3 `child.wait().await` reap + 500 ms guard (ITEM 1).**
  Pre-v1.1.8 `observe_topic_echo` spawned a `ros2 topic echo
  --once <topic>` subprocess per probe, called `child.kill()`,
  and dropped the handle without `wait()`. tokio's documented
  best-effort reaper (tokio-rs/tokio#2685) does not guarantee
  prompt release of per-`Child` state without an explicit
  `wait()`; v1.1.8 adds it with a 500 ms guard
  (`ROS2_CHILD_REAP_GUARD`, well below the 3 s per-probe
  `ROS2_SHELLOUT_TIMEOUT`). Empirical: ITEM 1 did NOT close the
  eventfd leak on its own — the eventfd growth rate is
  essentially unchanged (~1 FD/s). It does drain the child
  process cleanly (zero zombies in either run), so the fix is
  correct as a defensive measure but the candidate-(4) hypothesis
  is REFUTED as the principal driver. The follow-up dispatch
  needs `strace -e eventfd2` / `bpftrace ustack` / `lsof` to
  attribute the eventfd creation to its real source. Regression
  pin: `b3_echo_reaps_child_with_wait` (constant + invariant).

### Changed

- **Long-lived `sysinfo::System` + targeted memory refresh (ITEM 2).**
  Pre-v1.1.8 `platform::collect_system_metrics` built a fresh
  `sysinfo::System::new_all()` per tick and called `refresh_all()`
  — on Linux that walks every PID in /proc and allocates a
  sysinfo `ProcessSample`-equivalent per process AND does a
  global CPU usage refresh, despite the function reading only
  the memory fields off the `System`. Inspector estimated
  ~1000× alloc reduction for this fix alone. v1.1.8 makes the
  `System` long-lived (owned by `Runtime` via a new
  `sys_for_metrics` field, initialised by
  `platform::new_system_for_metrics()` with
  `RefreshKind::nothing().with_memory(MemoryRefreshKind::everything())`).
  Per-tick now calls only `sys.refresh_memory()`.

  CPU-count disposition: `sys.cpus().len()` on the targeted-
  refresh System returns 0 (the cpu list is only populated by
  `refresh_cpu_*`, verified against sysinfo-0.38.4 source). We
  use `std::thread::available_parallelism().map(|n|
  n.get()).unwrap_or(1)` — the std-library answer, no state,
  no extra refresh. Regression pin:
  `collect_system_metrics_with_targeted_refresh_populates_memory`
  (consecutive refreshes on the same `System` must agree on
  `total_memory`, `cpu_count > 0`). Touch confined to
  `src/platform/mod.rs` + `src/runtime.rs` (2 files).

### Known follow-up (STOP-AND-SURFACE — trigger 4)

- **Residual ~166 MB/min RSS growth + 1 FD/sec eventfd leak.**
  Above DISPATCH 25's 100 MB/min trigger; well above the
  Inspector target of <50 MB/min. The leak is NOT principally
  driven by tokio's per-`Child` reaper as DISPATCH 24
  hypothesised — adding `child.wait()` did not change the
  eventfd growth rate. Top remaining candidates the next
  dispatch should investigate, prioritised by allocation
  cadence (the leak grows at exactly tick-rate):
    1. tokio runtime scheduler park/unpark eventfds — verify
       worker thread count is stable per spawn
    2. tokio process driver inner signal-handler registration
       — even with `pidfd_open` for `Child`, an inner
       driver-side eventfd may be per-spawn
    3. mimalloc's per-thread state reclaim — mimalloc spawns
       a background thread under high churn; each owns an
       eventfd
    4. The audit writer's persistent file-handle wakeup
       mechanism
  Required diagnostic tools for the follow-up:
    - `strace -e trace=eventfd2,signalfd,timerfd_create -f -p $PID`
      captures eventfd creation real-time
    - `bpftrace -e 'tracepoint:syscalls:sys_enter_eventfd2 \
       /comm=="edge_monitor"/ { @[ustack(perf)] = count(); }'`
      attributes the creations to backtraces
    - `lsof -p $PID` shows the user-side opener of each FD

### Process notes

- **heaptrack is BLIND to v1.1.7+ allocations.** mimalloc
  bypasses libc malloc and mmap()s pages directly; heaptrack's
  LD_PRELOAD hook intercepts libc malloc only. The DISPATCH 25
  PHASE 0 run reported 290 KB peak heap / 72 KB leaked under a
  1.78 GB RSS run — useless as a decision gate. The diagnostic
  shifted to /proc-based snapshotting (smaps_rollup, fd type
  inventory, child process state, thread count, mmap region
  count) which observes kernel-side resource accumulation
  directly. Future leak-fix dispatches on v1.1.7+ binaries
  should either bring a `#[cfg(feature = "diag-glibc-alloc")]`
  toggle to swap mimalloc out for the diagnostic build, OR
  skip heaptrack and lean on /proc + syscall tracing.
- All three commits (ITEM 1, ITEM 2, polish) are independently
  bisectable.
- Empirical artefacts at `tests/empirical/v1_1_8/`:
  `heaptrack_baseline.txt` (the methodology-shift note + both
  heaptrack and /proc baseline numbers),
  `rss_endurance.txt` (5-min RSS + eventfd table for v1.1.7
  reference and v1.1.8 postfix, with the verdict + diagnostic
  tools the next dispatch needs).

## [1.1.7] — 2026-06-01 — close dispatcher clone-pressure leak

Closes the ~1.5 GB/min RSS leak that DISPATCH 19 surfaced after
the v1.1.6 Humble-compat hotfix. Tester-B's v1.1.5 observation
(120 MB → 9.5 GB over 12 min) reproduced on v1.1.6 at the same
order of magnitude (~1.35 GB/min) — the principal source was a
dispatcher allocation-pattern issue, not the Humble `--timeout`
flag. DISPATCH 21 Inspector root-caused it to per-(proc × source)
`Vec<ProcessSnapshot>` clone pressure introduced by v1.1.2's
two-slice expansion (semantically correct, allocation-pattern
wrong). DISPATCH 22 PHASE 0 heaptrack confirmed the prediction
verbatim (top-2 alloc stacks both rooted at the predicted line).

Empirical headline (10× ROS2 publishers, 5-min endurance):

  v1.1.6:               ~1,350 MB/min RSS growth (4.8 GB @ 180s)
  v1.1.7 Arc-share:       ~220 MB/min (~6.1× improvement)
  v1.1.7 +mimalloc:       ~190 MB/min (~7.1× improvement)

Top allocator (heaptrack baseline → postfix):
  v1.1.6: Dispatcher::tick:194 → Vec<ProcessSnapshot>::clone →
          HashMap<K,V>::clone (~258M of 409M alloc calls; 63%)
  v1.1.7: sysinfo::update_proc_info — normal /proc-scan cost
          (22.6M calls / 8.29 MB peak). Dispatcher::tick stack
          GONE from top-N.

Full empirical artefacts: `tests/empirical/v1_1_7/`.

### Fixed

- **Arc-share `ProcessSnapshot` slices in `Dispatcher::tick` (ITEM 1).**
  v1.1.2 (DISPATCH 7) broadened the dispatcher to pass BOTH the
  AI-filtered and the unfiltered process lists to each sample
  task so B2 could see bash tool-children. The implementation
  owned both `Vec<ProcessSnapshot>` lists per-tick and
  deep-cloned them per (proc × source) — and each
  `ProcessSnapshot` carries a `HashMap<String,String>` of the
  process's environ. With ~10 ROS2 PIDs × ~4 samplers and a
  few-hundred-PID host snapshot, per-tick clone volume was huge;
  per-source-mutex backpressure pinned multiple ticks' worth of
  clones live at once. v1.1.7 wraps the two slices in
  `Arc<Vec<ProcessSnapshot>>` so the per-task `.clone()` calls
  become refcount bumps. Trait surface unchanged — `&Arc<Vec<T>>`
  deref-coerces to `&[T]` at the `sample_with_context` call site.
  +~4 LoC. Regression pin
  `dispatcher_tick_shares_procs_via_arc_not_clone` asserts every
  spawned task in a tick receives the SAME backing buffer
  (probe sampler captures `all_procs.as_ptr() as usize`; 9 of 9
  invocations must equal).

### Added

- **`TelemetrySource::on_forget(pid)` trait method (ITEM 2).**
  Additive trait extension closing the cache-clear gap
  Inspector #15 surfaced and that v1.1.5 ITEM E shipped a
  bounded 5-min time-based GC workaround for. Default body is a
  no-op (most samplers are stateless per-PID). B3
  (`Ros2ShelloutSource`) overrides to drop the per-PID
  `PerPidState` entry. `Dispatcher::forget(pid)` spawns a tokio
  task per source that locks the per-source mutex and calls
  `on_forget(pid)`, so the runtime tick loop stays non-blocking.
  This is the `forget_pid` foundation extension DISPATCH 16
  trigger #4 deferred — now operator-sanctioned. Trait surface
  shape preserves the v1.1.2 `sample_with_context` signature
  unchanged; existing samplers inherit the no-op default. +3
  regression tests (default-noop, B3 override, dispatcher
  wire-through).
- **`mimalloc` global allocator (ITEM 3 fallback, operator-pre-approved).**
  Arc-share alone got to ~220 MB/min from ~1,350 MB/min; the
  residual was glibc allocator fragmentation under high
  short-lived-allocation churn (sysinfo /proc scans, JSON
  serialization, subprocess pipe buffers). mimalloc's per-thread
  free lists + aggressive coalescing + faster OS-return cadence
  shaves another ~13% (220 → 190 MB/min). `default-features = false`
  skips the secure-mode overhead.

### Known follow-up (STOP-AND-SURFACE)

- **Residual ~190 MB/min RSS growth.** Below the 1.5 GB/min FAIL
  bar (7.1× improvement) but NOT flat. The principal v1.1.6
  source (dispatcher Vec clones) is closed and confirmed by
  heaptrack. The remaining retention is a separate bug
  unrelated to the dispatcher clone path. Initial candidates
  (no diagnostic data yet — routed for a follow-up dispatch):
  unbounded mpsc backlog under sample-task overrun, accumulator
  per-PID growth that survives `forget()`, sysinfo internal
  process_map growth, tokio process-child handle accumulation,
  tracing log buffer growth. Recommend re-profiling under
  heaptrack with the v1.1.7 binary and filtering on per-tick
  allocations to find the second-tier source.

### Process notes

- DISPATCH 22 followed the PROFILE FIRST decision-gate protocol:
  Inspector's DISPATCH 21 hypothesis was confirmed by heaptrack
  BEFORE the Arc-share fix shipped. Top-2 alloc stacks rooted
  at `dispatcher.rs:194` exactly as predicted (~63% of alloc
  calls). Future leak-fix dispatches should keep this gate.
- Empirical artefacts at `tests/empirical/v1_1_7/`:
  `heaptrack_baseline.txt` (v1.1.6 profile with header
  + raw output), `heaptrack_postfix.txt` (v1.1.7 Arc-share
  profile, same shape), `rss_endurance.txt` (5-min RSS table:
  baseline / Arc-share / Arc+mimalloc).
- All three commits are independently bisectable
  (Arc-share / on_forget / mimalloc).
- mimalloc is the first dependency-addition shipped under
  operator pre-approval. The approval was scoped to THIS
  fallback only; future allocator swaps or dep additions need
  fresh operator approval.

## [1.1.6] — 2026-06-01 — v1.1.5 Humble-compat hotfix

Hotfix release for the v1.1.5 BUG-P5-2 ship regression: every
`ros2 topic echo` probe failed on Humble (`unrecognized arguments:
--timeout`) and every ROS2 row locked to Idle. The v1.1.5 tag was
retracted (local + remote, `git tag -d v1.1.5` + `git push origin
:refs/tags/v1.1.5`); the merge commit is preserved on the branch
and this release fixes forward. Caught by Tester-B (DISPATCH 17B).

Two demonstrated fixes shipped; two follow-on items
(STOP-AND-SURFACE under the dispatch's architectural-issue clause)
left open for a follow-up dispatch — see "Known follow-ups" below.

### Fixed

- **CRITICAL — `--timeout` flag dropped from B3 echo invocation
  (ITEM 1).** Humble's `ros-humble-ros2cli 0.18.18` does NOT
  support `--timeout` on `topic echo` (the flag was added in
  Iron / Jazzy / Rolling). v1.1.5 shipped with `ros2 topic echo
  --once --timeout <T> <topic>` and every probe failed with
  `unrecognized arguments: --timeout` → `last_message_at` never
  updated → every ROS2 topic locked to Idle. v1.1.6 invokes
  `ros2 topic echo --once <topic>`; the per-probe cap is now the
  outer `ROS2_SHELLOUT_TIMEOUT` (3 s tokio wrap) plus `--once`
  self-termination. Verified on the v1.1.6 dev host —
  `ros2 topic echo --help` lists `--once` but not `--timeout`.
  The dead `ROS2_ECHO_PROBE_TIMEOUT` constant was removed; the
  module-header doc-comment records the v1.1.6 ITEM 1 rationale.
  ~10 LoC. Regression-pin test:
  `b3_echo_once_no_timeout_flag_detects_active_topic` — args
  extracted into `ros2_echo_args`; test asserts the list contains
  `--once` and the topic and does NOT contain `--timeout`.

### Changed

- **B3 production-shape harness mirrors the v1.1.6 echo-once
  shape (ITEM 2).** Closes the harness-drift gap that hid the
  v1.1.5 regression. The pre-v1.1.6 `b3_ros2_harness.sh` tested
  `ros2 topic hz` (the v1.1.4 mechanism) and passed green against
  the broken v1.1.5 echo-once subprocess — because it never
  invoked that shape. The rewrite spawns the EXACT B3 invocation
  against a live publisher AND adds a Humble-compat GUARD step
  that asserts `ros2 topic echo --once --timeout 1 <topic>` STILL
  fails on the host (so a future ros2cli backport adding the flag
  trips the harness for re-evaluation). First live-validated state
  of the B3 harness; v1.1.3 shipped it DRAFTED-not-live-validated.
  Live run on the dev host:
  `GUARD OK: Humble ros2cli rejects --timeout on topic echo (rc=2).
  rate=1Hz first-message=.847s margin=2.15s under the 3s inner.`
  `tests/integration/sampler_harnesses/README.md` updated: bug-table
  row added for the v1.1.5 ship bug; B3 examples re-shaped around
  the v1.1.6 invocation; `INNER` default lowered 8 s → 3 s.
- Also fixed a `set -e` trap in the harness: `((wait_count++))`
  returns 0 the first iteration and trips `set -e`; replaced with
  the arithmetic-assignment form.

### Known follow-ups (STOP-AND-SURFACE)

The DISPATCH 19 work surfaced two items the hotfix scope cannot
absorb without a deeper investigation pass. Both are filed as
follow-up dispatch input rather than shipped fixes:

- **RSS growth — ITEM 3 Step C.** Tester-B observed v1.1.5
  growing 120 MB → 9.5 GB over 12 min (~1 GB/min) against ~10
  live ROS2 publishers. ITEM 3 Step A re-measured v1.1.6 ITEM 1
  on the same host: 8.4 GB at ~5.5 min (~1.5 GB/min) — leak
  survives the `--timeout` fix and is at the same order of
  magnitude whether echo probes succeed (v1.1.6) or fail fast
  (v1.1.5). Initial code-reading flags `Dispatcher::tick` in
  `src/telemetry/dispatcher.rs:170-233` as a likely contributor:
  every (proc × source) combination clones the full `all_procs`
  and `ai_procs` `Vec<ProcessSnapshot>`s for its spawned task.
  With ~10 ROS2 PIDs × 4–5 samplers per tick and a few hundred
  PIDs per snapshot, per-tick clone volume is large; if per-source
  mutex backpressure pushes pending tasks across ticks, multiple
  ticks' worth of clones accumulate. Candidate fix: switch the
  per-task `Vec<ProcessSnapshot>` clones to `Arc<Vec<…>>` so all
  tasks of a tick share one allocation. Confidence: medium —
  consistent with the slope but not heap-profiler-confirmed.
  Routed to a follow-up dispatch for heap-profiling +
  architectural review (the user has flagged dispatcher rework
  as larger than a hotfix can absorb).
- **Killed-PID ghost rows — ITEM 4.** Operator observed rows
  for killed publishers persisting 10+ s in the workloads panel.
  Initial investigation: `src/lifecycle/tracker.rs` correctly
  clears exited PIDs from `self.previous` after the post-exit
  tick (the agent hypothesis that lifecycle was the source is
  disproven by reading lines 33–69 directly). The persistence is
  elsewhere — most likely a stale read from
  `RuntimeState::live_telemetry` (`src/runtime.rs`) or the
  wire-layer cache (`src/web/wire.rs`). The
  `forget_pid`-trait-method approach was deliberately avoided
  in v1.1.5 ITEM E (DISPATCH 16 trigger #4 territory); whether
  this fix needs that foundation extension depends on where the
  retention actually lives. Routed to a follow-up dispatch.

### Process notes

- v1.1.5 tag retracted (`git tag -d v1.1.5` + `git push origin
  :refs/tags/v1.1.5`); merge commit preserved on the branch
  (second retraction in the v1.1.x series, after v1.1.0 — same
  fix-forward protocol).
- The dispatch's STOP-AND-SURFACE clause was exercised for the
  first time on architectural-feeling items; see "Known
  follow-ups" above.

## [1.1.5] — 2026-06-01 — cleanup bundle + BUG-P5-2 fix

Five-item bundle closing the audit DRIFT findings from the
2026-06-01 dispatches plus an Inspector side-finding. The
architectural BUG-P5-2 fix lands here under operator-locked
APPROACH (c) — Inspector's default.

### Fixed

- **D-DAEMON-MARKER — daemon classifier marker matches real Humble
  shape (ITEM A).** v1.1.4's `_ros2_daemon` marker missed the
  daemon's real argv on current Humble
  (`python3 -c "from ros2cli.daemon.daemonize import main; main()"
  --name ros2-daemon`). Added the stable module path
  `ros2cli.daemon.daemonize` to `ROS2_CLI_INTROSPECTION_MARKERS`
  alongside `_ros2_daemon`; either form trips the guard.
- **D-BUG-P5-2-STALE-TEXT — stale deferral version (ITEM C).** B3
  doc-comment said BUG-P5-2 was "deferred to v1.1.4"; flipped to
  v1.1.5 to match the actual resolution path. ~2 LoC.
- **D-B4-SCRIPT-ASYMMETRY — foundation extension (ITEM B).**
  v1.1.4 broadened the classifier (script-sniff + extended keyword
  coverage) but B4 still gated on its own
  `is_embeddings_cmdline` — script-file embeddings workloads
  classified correctly but were never sampled (activity null).
  Operator-locked APPROACH α: foundation extension. Added
  `ProcessSnapshot.workload_category: Option<WorkloadCategory>`
  (third additive field after DISPATCH 1.5 `cpu_pct` and 1.6
  `ppid`); the runtime builders plumb it from
  `AnnotatedProcess.workload_category`; B4's `applies_to` reads
  it directly. The cmdline-substring duplication
  (`EMBEDDINGS_CMDLINE_MARKERS` + `is_embeddings_cmdline`,
  ~40 LoC) is retired. 11 construction sites updated.
- **BUG-P5-2 — sub-Hz ROS2 topics now observable (ITEM D).**
  Architectural fix under operator-locked APPROACH (c) —
  Inspector's default. Replaced per-tick `ros2 topic hz` (whose
  first-emit time scales with 1/rate, structurally unobservable
  at ≤0.5 Hz) with `ros2 topic echo --once --timeout 1` per tick +
  a 30 s per-topic staleness window. Echo arrival is observable
  at any non-zero rate; the window covers ≥3 expected arrivals at
  0.1 Hz, so a single observed message holds Active across the
  inter-message gaps. ~-40 LoC net (removed: hz regex parser,
  WARNING fast-fail, hz observation/read helpers, hz cache fields,
  5 hz-rate parser tests; added: echo helpers, per-topic
  `last_message_at`, sub-Hz regression pin).
  Surfaced (NOT touched in this release per trigger #3):
  `DESIGN_HANDOFF.md:120`'s `{Hz}` field in the ROS2 row template
  is aspirational; B3 has never emitted a rate field on the wire.
  Updating the template text is a separate contract change.
- **B3 cache GC — Inspector side-finding (ITEM E).** The Inspector
  flagged that `dispatcher.forget(pid)` doesn't propagate to B3's
  per-PID cache — a bounded leak today, more material under the
  v1.1.5 echo-once mechanism's per-topic state. Operator's
  enumerated options included (a) a `forget_pid` trait method,
  which explicitly trips STOP-AND-SURFACE trigger #4
  ("foundation work that may warrant separate dispatch"). Picked
  a third in-scope path: B3-side time-based GC. Sweeps
  `last_topic_list_at` against a 5-minute threshold
  (10× the topic-list refresh interval) at the top of each
  `sample`. Bounds the leak in time, not in PID count — equivalent
  closure property to a dispatcher hook, no trait extension. The
  inherent `Ros2ShelloutSource::forget(pid)` is in place for
  future dispatcher wiring if APPROACH (a) is later sanctioned.

### Surface — flagged but not modified

- **`DESIGN_HANDOFF.md:120` `{Hz}` field.** Aspirational template
  text; never produced by B3 on the wire (B3 emits only
  `activity_state`). Updating to remove the field is a visible
  contract change worth its own dispatch (trigger #3).

### Test count: 896 → 898 (+2 net)

- ITEM A: +1 (real Humble daemon shape)
- ITEM B: +3 added applies_to tests (workload_category-based)
  −1 absorbed (the `applies_to_rejects_non_embeddings_python`
  test became `applies_to_rejects_none_category`)
- ITEM D: +3 (sub-Hz regression pin, staleness-window constants
  pin, forget inherent test) −5 (obsolete hz-rate parser tests)
- ITEM E: +2 (GC sweep predicate, threshold-vs-refresh-interval
  pin)

## [1.1.4] — 2026-05-24 — bug-surface fixes (P5 + DISPATCH 11 carry-forward)

Narrow-scope hotfix closing the carry-forward items from P5 +
DISPATCH 11. Four fixed; one (sub-Hz ROS2) surfaced as
architectural and deferred to v1.1.5.

### Fixed

- **DISPATCH 11B — b1 harness model_name filter.** The b1
  integration harness read its activity signal with a filter that
  could select the ollama *daemon* row (no model, activity None)
  instead of the *runner* row, producing a false-negative exit
  against a working B1. Added the `model_name` guard, mirroring
  the classified-name filter. Harness-only.
- **BUG-P5-1 — ROS2 daemon / CLI over-classify.** Read-only `ros2`
  CLI introspection commands (`ros2 topic hz`, `ros2 node list`,
  …) and the `_ros2_daemon` helper were classifying as ROS2
  workloads (transient `ros2` rows in the operator's screenshot)
  because the `ros2` CLI imports rclpy → loads librcl → fires the
  library signal. New `is_ros2_cli_introspection` early-return
  guard (same shape as the v1.0.2 tooling-name / shell-wrapper
  guards). Node-spawning (`ros2 run`/`launch`) and
  traffic-generating (`ros2 topic pub`) verbs deliberately keep
  classifying.
- **P5-B4-CLASSIFY — embeddings classifier coverage.** Embeddings
  workloads whose model family wasn't `sentence-transformers` /
  `bge-` fell through to Unknown. Broadened the classifier (and
  the B4 sampler markers, kept in sync) with high-confidence
  family / library substrings: FlagEmbedding, gte-,
  e5-{base,large,small}, multilingual-e5, nomic-embed, all-MiniLM,
  jina-embeddings, plus FlagEmbedding source imports in
  script-sniff. Chose name/cmdline coverage over a /proc/maps
  library signal (embeddings share libtorch — no unique .so) and
  over a CPU-magnitude heuristic (would false-positive on training
  jobs). A no-import CPU-only proxy remains Unknown by design.
- **P5-ENV-ROS — `ROS_DOMAIN_ID` necessary-but-not-sufficient.**
  A process launched from a ROS-sourced shell inherits
  `ROS_DOMAIN_ID` and was false-classified as ROS2 (Tester-B:
  an embeddings process classified ROS2 until `env -u
  ROS_DOMAIN_ID`). MEDIUM-HIGH: on the Jetson target (ROS env
  globally sourced) every inheriting process would misclassify.
  `classify()` now requires a standalone-trustworthy signal
  (cmdline marker or `/proc/maps` library link) to classify ROS2;
  `ROS_DOMAIN_ID` only enriches evidence when such a signal
  already fired. Genuine ROS2 nodes are unaffected (they load
  librcl / carry a node-spawn marker).

### Surfaced — deferred to v1.1.5 (architectural)

- **BUG-P5-2 — sub-Hz ROS2 topics.** Topics ≤ 0.5 Hz (0.1 Hz needs
  ~29 s probe) remain structurally unobservable; raising the
  timeout further punishes healthy 1 Hz cases. All three fix
  approaches (streaming hz monitor, direct DDS/rclrs subscription,
  message-timestamp observation) redesign how B3 acquires its rate
  signal — the DDS approach would also need a new dependency
  (foundation no-new-deps lock). STOP-AND-SURFACED per the
  dispatch's architectural trigger; routed to a v1.1.5
  architectural dispatch.

### Test count: 888 → 896 (+8)

- ITEM 2 (ROS2 introspection guard): +5
- ITEM 3 (embeddings coverage): +2
- ITEM 5 (ROS_DOMAIN_ID): +1 net (3 new ros2.rs tests, 2 replaced;
  3 classifier/mod.rs tests flipped in place)

## [1.1.3] — 2026-05-24 — P5 refinements + integration harness (CI deferred)

Phase 2 closeout. Empirically-anchored sampler refinements from P5
sampler validation (DISPATCH 9A + 9B). The production-shape
integration harnesses Tester-A built during P5 are promoted into
the tracked repo at `tests/integration/sampler_harnesses/` for
local developer use; CI smoke-stage wiring is deferred pending
self-hosted runner infrastructure (stock CI lacks ollama / claude /
ROS2).

### Refined (P5 empirical data)

- **B3 `ROS2_SHELLOUT_TIMEOUT`: 5 s → 8 s.** P5 measured `ros2
  topic hz` first-emit at ~4.90 s for 1 Hz topics (only 0.10 s
  headroom under the old 5 s ceiling — empirically marginal) and
  ~6.83 s for 0.5 Hz. 8 s gives a 1.6× margin and clears 0.5 Hz.
- **B3 `sample_timeout()`: 6 s → 9 s** (inner+1s convention; +1 s
  for subprocess kill-signal propagation). The `outer > inner`
  invariant — the v1.1.0 B3 root-cause guard — is unchanged.
- **B4 `EMBEDDINGS_WINDOW_SAMPLES` (3 samples) →
  `EMBEDDINGS_IDLE_WINDOW` (12 s).** Tester-B confirmed the
  CPU-percent signal is correct (idle ~0% vs active 170-800%), so
  the 60.0 threshold is PRESERVED; the count-based ~3 s window was
  too short to bridge embeddings' ~5 s inter-burst gaps and
  flickered Active↔Idle. The 12 s duration hold-window (HashMap
  `last_active_at` pattern mirroring B2's `AGENT_IDLE_WINDOW`)
  bridges the gaps without false-Active for genuinely-idle
  workloads.

### Added

- **`tests/integration/sampler_harnesses/`** — production-shape
  integration harnesses for B1 / B2 / B3. They spawn / inspect
  *real* workloads through a running `edge_monitor` and assert the
  sampler's identity assumption against the *real* data slice,
  reproducing the v1.1.0 B1 (digest-vs-name) and v1.1.1 B2 (bash
  child filtered out of `ai_procs`) historical bugs that
  hand-built symmetric fixtures could not. Local-run only; see the
  directory README for the procedure and host dependencies.

### Deferred

- **CI smoke-stage wiring for the harnesses** — stock CI runners
  lack the production deps (ollama / claude / ROS2); an
  unconditional job would red-flag every PR. The structural fix is
  the harness *availability* for local pre-PR runs; CI automation
  is a multiplier deferred to a self-hosted-runner dispatch.
  `.github/workflows/ci.yml` is unchanged this release.

### Carried forward to v1.1.4 (bug surface from P5)

- **BUG-P5-2** — sub-Hz ROS2 topics (≤ 0.5 Hz at the lowest rates)
  remain structurally unobservable even at the 8 s timeout; the
  real fix is windowing / streaming the hz output, not a larger
  timeout.
- **BUG-P5-1** — ROS2 daemon helpers / CLI over-classify.
- **P5-B4-CLASSIFY** — B4 misses non-argv model identifiers.
- **P5-ENV-ROS** — `ROS_DOMAIN_ID` env inheritance misclassifies
  non-ROS workloads (medium-high; relevant to the Jetson Orin
  deployment target).

### Test count: 885 → 888 (+3)

- B3 timeout refinement: 0 net (existing pin test updated in place).
- B4 hold-window: +3 net (removed 2 count-window tests, added 5
  hold-window tests).

## [1.1.2] — 2026-05-24 — B2 active-detection fix + trait expansion

Hotfix for the B2 active-detection bug surfaced by DISPATCH 6B
paired Tester validation of v1.1.1. v1.1.1 fixed B2 *classifier*
coverage (the local `.vscode/extensions/` path), but the
*sampler* still couldn't detect bash tool-children: the runtime
filters NotAi processes (including bash) out of the process list
before passing it to the dispatcher, so B2's child scan always
came up empty and activity locked to Idle.

### Fixed

- **B2 Agent (claude) active-detection.** bash tool-children are
  `NotAi`-classified and were excluded from the runtime's filtered
  process list, so `has_bash_child` was always false and the
  sampler emitted Idle even while the agent was actively running
  a Bash tool. Fix: expand `TelemetrySource::sample_with_context`
  with separate `ai_procs` (filtered) and `all_procs` (unfiltered)
  parameters; B2 reads `all_procs` for child detection.

  This is the same defect class as the B1 v1.1.0 asymmetric
  compare and the cpu_pct / ppid foundation gaps: the data
  existed, the plumbing landed (DISPATCH 1.6 added
  `ProcessSnapshot.ppid` specifically for this check), but the
  consumer was reading a list that excluded the relevant ppids.

### Trait API change (additive; default polyfill preserves backward-compat)

- `TelemetrySource::sample_with_context` signature gains an
  `all_procs: &[ProcessSnapshot]` parameter (the previous
  `all_procs` is renamed `ai_procs`). The default polyfill still
  discards both lists and delegates to `sample`, so a sampler
  that doesn't override the method is unaffected.
- `Dispatcher::tick` gains a second `all_procs` parameter; the
  runtime builds an unfiltered `all_live` list (the existing
  `live_ai` builder minus the NotAi skip) and passes both.
- Existing samplers (B1, B3, B4, vLLM, llama.cpp) do NOT override
  `sample_with_context` — they inherit the polyfill and needed no
  change. Only B2 reads the new `all_procs`.

### Discipline

- Asymmetric-fixture discipline applied to this bug class: the
  new `sample_with_context_active_via_unfiltered_bash_child` test
  uses an `ai_procs` that EXCLUDES the bash child and an
  `all_procs` that INCLUDES it. It would have FAILED on v1.1.1
  and PASSES on v1.1.2 — the regression pin for the DISPATCH 6B
  finding. Companion negative
  (`sample_with_context_idle_when_bash_child_absent_from_all_procs`)
  confirms the Active verdict is driven by bash-child presence.

### Carried forward to v1.1.3 via P5

- B4 PROVISIONAL thresholds still need an embeddings-specific
  empirical anchor.
- All other PROVISIONAL items from v1.1.1 carry forward (per-PID
  HashMap slow leaks, B1 no `Loading` state).

### Test count: 883 → 885 (+2)

## [1.1.1] — 2026-05-24 — first usable Phase 2 release

v1.1.0 was tagged but did NOT validate (DISPATCH 4 + Tester-2
corroboration: B1 Ollama and B3 ROS2 both locked to
`NotDetected`; B2 Agent could not be validated due to a
classifier coverage gap). v1.1.0 tag retracted from origin
before this release. **v1.1.1 is the first shippable Phase 2
release.**

### Fixed (root causes)

- **B3 timeout architecture — pre-v1.1.1 dispatcher cancelled
  long-running samplers.** The dispatcher used a single global
  1 s outer timeout on every `sample_with_context`. B3's inner
  `ROS2_SHELLOUT_TIMEOUT = 5 s` was always cancelled at 1 s, so
  `ros2 topic hz` never observed the ≥ 3 published messages it
  needs to emit a rate. Every ROS2 row locked to NotDetected.

  Fix: new `TelemetrySource::sample_timeout(&self) -> Duration`
  trait method with a default body returning
  `DEFAULT_SAMPLE_TIMEOUT` (1 s). Existing samplers (vLLM,
  llama.cpp, Ollama, B2, B4) inherit the default — no behaviour
  change. B3 overrides to `Duration::from_secs(6)` (5 s inner +
  ~1 s kill-signal headroom).

  Dispatcher's `sample_timeout` field became `Option<Duration>`;
  `None` (default) means "ask the sampler under the lock"; the
  existing `with_sample_timeout` helper still works as a
  host-wide override and is exercised by the slow-sampler
  protection test.

- **B1 Ollama match — pre-v1.1.1 asymmetric compare locked
  every runner to NotDetected.** At `ollama_api.rs:320` the
  runner branch tested `loaded.iter().any(|m| m == my_model)`,
  where `loaded` carried friendly names from `/api/ps`
  (`"smollm:135m"`) and `my_model` carried the classifier-
  extracted blob digest (`"sha256-eb2c714d40d4..."`). Never
  matched. The original unit test used `"tinyllama:latest"` on
  both sides — a same-string fixture that masked the real-world
  asymmetry.

  Fix (option (i)): replace per-model matching with
  `!loaded.is_empty()`. Ollama runner subprocesses exist iff
  Ollama has a model loaded — 1:1 relationship — so `/api/ps`
  non-empty IS the presence signal. Each runner reads its OWN
  `proc.cpu_pct` for the bimodal verdict. Per-model state still
  keyed by `my_model` (blob digest) so CHANGE 14 (runner
  re-spawn under VRAM pressure preserves streak) holds.

  Option (ii) (`/api/show` digest lookup) rejected: adds an
  HTTP call per loaded model per tick for granularity the
  per-runner CPU% decision doesn't need.

- **B2 Agent classifier — local VS Code install layout not
  matched.** `SAAS_LLM_CLI_PATTERNS` covered only the VS Code
  Remote-SSH layout (`vscode-server/extensions/...`).
  DISPATCH 4 ran on a host with a local install
  (`~/.vscode/extensions/anthropic.claude-code/`); classifier
  fell through to NotAi, so B2's `applies_to` never fired and
  the sampler could not be validated.

  Fix: extend the allowlist with the local-install pattern for
  each currently-supported tool. Eight entries total (was five).
  No change to B2 itself; B2 code may work and validation
  unblocks in DISPATCH 6.

### Discipline (STEP 5)

- Asymmetric-fixture audit: 121 sampler-area tests reviewed,
  1 rewritten (B1's `empty_models_yields_not_detected_for_known_runner`
  now uses realistic asymmetric strings; new regression pin
  `asymmetric_runner_digest_vs_api_ps_friendly_name_classifies_active`
  added). Other samplers use single-source fixtures or
  compare symmetric-in-real-world types — no rewriting needed.
  Discipline note + `// SYMMETRIC: real-world is also symmetric`
  idiom documented above `src/telemetry/samplers/ollama_api.rs`'s
  test module.

### Carried forward

- B1, B2, B4 thresholds remain PROVISIONAL; P5 sampler
  validation will refine.
- B2 and B4 per-PID HashMaps are still bounded slow leaks;
  dispatcher cleanup hook deferred to v1.1.2.
- B1 still does not emit `Loading` (`/api/ps` has no load
  timestamp); v1.2+ revisit for larger models.

### Test count: 876 → 883 (+7)

- STEP 2 (B3 timeout): +4 (3 dispatcher tests + 1 B3 pin)
- STEP 3 (B1 match): +1 net (1 rewritten in place + 1 new
  regression pin)
- STEP 4 (B2 classifier): +2
- STEP 5 (audit): 0 (documentation-only)

### v1.1.0 retraction

v1.1.0 tag (commit a7a3169) was deleted from origin before
v1.1.1 was tagged. Downstream consumers that fetched v1.1.0
keep their local copy; the remote tag list shows only v1.1.1+
going forward.

## [1.1.0] — RETRACTED (see v1.1.1)

This release was tagged on 2026-05-24 but did not validate
under DISPATCH 4 + Tester-2 corroboration. The entry below is
preserved for audit traceability; the tag has been removed
from the remote. v1.1.1 is the first shippable Phase 2 release.

Phase 2: per-category activity surfacing. v1.0.x told operators
which workload was alive on the box; v1.1.0 tells them whether
each workload is **doing observable work right now**
(`Active` / `Idle` / `Loading` / `NotDetected`).

Four new per-category samplers + the foundation surface they
plug into. Wire schema is additive at the field level — every
v1.0.x `RunRecord` JSON round-trips through a v1.1 reader.

### Foundation (DISPATCH 1 + 1.5 + 1.6)

- **`ActivityState` enum** — `Active` / `Idle` / `Loading` /
  `NotDetected`. Local to edge_monitor for v1.1.0; CAR-candidate
  for `ux_contract` v0.3.12 once Phase-2 sampler validation
  (P5) confirms the four-variant taxonomy holds. Foundation
  commit on `phase2-foundation` (merged at `9db8639`).
- **`TelemetryFrame::activity_state: Option<ActivityState>`** —
  additive wire field with `#[serde(default)]` so a v1.0
  reader round-trips a v1.1 frame and vice versa.
- **`TelemetrySource::sample_with_context`** — additive trait
  method with a default polyfill that delegates to `sample`.
  Lets B2 inspect the full per-tick process list to detect
  Bash-tool subprocesses without breaking the existing
  single-PID `sample` contract for every other sampler
  (Inspector #12 Option (i) + operator Q3 lock).
- **`ProcessSnapshot::cpu_pct: f32`** — DISPATCH 1.5
  (`5b448dd`). Raw `0-(100×cores)` scale documented inline at
  `src/telemetry/source.rs:23-43`. Empirical anchor from
  Tester-B's `/api/generate` capture: an Ollama runner during
  generation pins ~1 core sustained (bimodal 99-105% vs
  0-1% idle).
- **`ProcessSnapshot::ppid: Option<u32>`** — DISPATCH 1.6
  (`526555f`). Surfaced during DISPATCH 2A B2 work as a second
  foundation gap with the same shape as `cpu_pct`. Enables B2's
  multi-instance attribution: without it, 22 concurrent claude
  agents would each be credited with every bash in the
  snapshot.
- **`Dispatcher::activity_for(pid)`** accessor on the
  per-PID accumulator.
- **TUI workloads column** — 8-char text-label only per
  Inspector #8 V1 + L21 §14. No per-state colors or spinners
  in v1.1.0; those land in v1.1.x once P5 validates the four
  state semantics.

### Per-category samplers (4)

- **B1 — Ollama activity** (DISPATCH 2A; `OllamaApiSource`
  extended in place per Inspector #12 + operator Q2). Bimodal
  CPU% detection on the Ollama runner subprocess:
  `OLLAMA_ACTIVE_CPU_PCT = 50.0` (EMPIRICAL — Tester-B's
  220-sample 5-50% empty band), 2-sample idle debounce
  (`OLLAMA_IDLE_DEBOUNCE_SAMPLES = 2`). Runner PID re-resolved
  every tick (no caching — Ollama silently evicts and
  re-spawns runners under VRAM pressure). Connection-refused
  emits `NotDetected` (not `Transient`) to suppress stale
  Active state during daemon outages; once-log on Up↔Down
  transitions. `Loading` state explicitly NOT emitted in
  v1.1.0 — `/api/ps` carries no load timestamp; Tester-B
  verified models go absent → present in a single ~1.4 s
  tick for small models. Rejected signals (nvidia-smi GPU
  util, `--query-compute-apps` memory, `pmon -s u`,
  `/api/generate` poll) documented in source.
- **B2 — Agent (claude)** (DISPATCH 2A; new
  `AgentClaudeSource`). Uses `sample_with_context` +
  `child.ppid == agent_pid` filtering to detect Bash-tool
  subprocesses. `applies_to` is two-factor + two-reject:
  `basename(argv[0]) == "claude"` AND argv contains the
  two-token `--output-format` `stream-json`; rejects
  `.claude/shell-snapshots/` (Bash-tool subshells) and any
  match relying on `comm` or `/proc/exe` (multi-call binary
  recursion guard — Tester-A verified 1 of 22 claude-binary
  processes had `argv[0] = ugrep` and would have false-fired
  a non-argv\[0\] check). `AGENT_IDLE_WINDOW = 60s`
  (PROVISIONAL).
- **B3 — ROS2 (shellout)** (DISPATCH 2B; new
  `Ros2ShelloutSource`). `tokio::process` shellout to `ros2
  topic list` + `ros2 topic hz <topic>`. Two-line
  TAB-indented Hz parser; 5 s inner `tokio::time::timeout`;
  WARNING-line fast-fail when a topic has no publisher.
  Carries an `EDGE_MONITOR_SAMPLER` env-var marker on
  spawned subprocesses so a future classifier-side
  recursion guard can recognise B3's own probes.
- **B4 — Embeddings (CPU heuristic)** (DISPATCH 2B; new
  `EmbeddingsCpuSource`). Sustained-CPU window over
  `ProcessSnapshot::cpu_pct`; `max(window) ≥
  EMBEDDINGS_ACTIVE_CPU_PCT = 60.0` → Active.
  `EMBEDDINGS_WINDOW_SAMPLES = 3` rolling buffer absorbs
  burstiness (sentence-transformers / BGE / GTE / E5
  workloads encode in 100-300 ms then idle). PROVISIONAL
  thresholds — no empirical capture yet.

### Empirical data

Pre-implementation captures preserved under
`tests/empirical/v1_1_0_prep/`:

- `ros2_shellout_format/` (Tester-A; anchors B3)
- `claude_agent_format/` (Tester-A; anchors B2 — verified
  the ugrep-symlink recursion case and the two-token
  `--output-format stream-json` form)
- `ollama_api_format/` (Tester-B; anchors B1's schema-guard
  test)
- `ollama_generate_sidechannel/` (Tester-B; anchors B1's
  bimodal CPU threshold)

### Known limitations (PROVISIONAL v1.1.1 refinements)

- B1 / B2 / B4 thresholds are locked from one host's
  empirical data; P5 sampler validation will refine.
- B2 and B4 per-PID HashMaps (`last_active_at`,
  `per_pid_cpu_window`) are bounded slow leaks. The
  dispatcher does not invoke a per-PID cleanup hook on
  `SourceError::Permanent`; growth is bounded by observed
  PID count (~50 bytes/PID, dozens concurrent empirically).
  Dispatcher cleanup hook deferred to v1.1.1.
- `Loading` state is NOT emitted by B1 in v1.1.0. Side-channel
  detection of cold-start (runner subprocess appearance,
  nvidia-smi compute-app appearance, `/api/generate`
  correlation) deferred to v1.2+ if it becomes worth the
  complexity for larger models.
- B3 and B4 cmdline-detect only — library-signal-only ROS2
  nodes (C++ nodes spawned without `ros2 run` and without
  `ROS_DOMAIN_ID`) and library-signal-only embeddings
  workloads classify in the panel but get no Phase-2
  sampling. Acceptable v1.1.0 gap; v1.1.1+ can plumb
  `workload_category` onto `ProcessSnapshot` to close.

### Wire schema

`TelemetryFrame.activity_state` is additive with
`#[serde(default)]`. Schema version unchanged.

### Test count: 833 → 876 (+43)

- B1: +8 in `ollama_api`
- B2: +12 in `agent_claude`
- B3: +13 in `ros2_shellout`
- B4: +10 in `embeddings_cpu`

## [1.0.4] — 2026-05-23

Documentation-only release closing the 7 pre-existing `cargo doc`
warnings surfaced post-v1.0.3 (Inspector #11 audit), wiring a
permanent CI gate against doc-warning regression, and answering
two documentation gaps Tester-A flagged during v1.0.3 validation.
No source behaviour change. Test count unchanged at 822.

### Changed (doc-only)

- **W1 `src/config.rs:70`** — `[\`telemetry::Dispatcher\`]` →
  `[\`crate::telemetry::Dispatcher\`]` (path qualification).
- **W2 `src/governor/pid_reuse.rs:38`** — `argv[0]` → `` `argv[0]` ``
  (rustdoc was parsing the literal brackets as a link target).
- **W3 `src/model.rs:12`** — same fix as W2 on the
  `ProcessSample::cmdline` doc-comment.
- **W4 `src/runtime.rs:119`** — `[\`ExitReason\`]` →
  `[\`crate::storage::run_store::ExitReason\`]` (path qualification;
  the function body imports `ExitReason` but the outer doc-comment
  doesn't inherit the import).
- **W5 `src/telemetry/vision_probe.rs:11`** — de-linked
  `AGGREGATION_WINDOW` to a bare backtick form, matching the
  existing convention at lines 69 / 195 / 200 in the same file.
  The constant is module-internal, not stable API.
- **W6 `src/ui/panels/alerts.rs:4`** — `[\`AlertState::visible\`]` →
  `[\`crate::ui::alerts::AlertState::visible\`]`.
- **W7 `src/ui/panels/header.rs:81`** — de-linked
  `MIN_TIME_GAP_COLS` (module-internal layout constant).

### Added

- **CI gate against doc-warning regression.** New step in
  `.github/workflows/ci.yml` runs `cargo doc --workspace --no-deps`
  with `RUSTDOCFLAGS: -D warnings`. The Inspector #11 dispatch
  documented the gate as `cargo doc … -- -D warnings` but that
  syntax fails — cargo doc has no `--` separator for rustdoc
  flags; the rustdoc-flag channel is the `RUSTDOCFLAGS` env var.
  CI step uses the working form.
- **§20 — Wire snapshot observable surface** added to
  `DESIGN_HANDOFF.md`. Documents what `/api/snapshot` workload
  rows actually expose (`pid`, `name`, `model_name`, `category`,
  `workload_category`, resource fields, status), what they do NOT
  expose (`cmdline`, full process tree, `/proc/<pid>/maps`), and
  the `TASK_COMM_LEN=16` kernel-truncation rule on `name`.
  Closes Tester-A's F1 finding.
- **`Known gotchas` section** added to `CLAUDE.md` documenting
  the `ros2 launch` SIGTERM-propagation gotcha (it does not call
  `setsid` before spawning its children, so SIGTERM propagates
  across the session group and can kill `edge_monitor` if both
  share a terminal). Workaround documented: wrap in `setsid` or
  use a separate session. Verified that the demo at
  `edge_monitor_demo.sh` uses `ros2 run rclcpp_components
  component_container`, NOT `ros2 launch`, so it is unaffected
  and does not need updating. Closes Tester-A's F2 finding.

### Wire schema

Unchanged — still v0.1.

## [1.0.3] — 2026-05-22

Hotfix release closing two platform-layer bugs that were silently
shaping every v1.0.x record — rclpy Python ROS2 nodes invisible to
the classifier (B-EMPIRICAL-4) and every CUDA workload reporting
zero VRAM (B-VRAM-ZERO) — plus four small hygiene rides. No wire
schema change; no behaviour change for processes already classified
correctly under v1.0.2.

### Fixed

- **B-EMPIRICAL-4 (Tester-A + Inspector #9):** rclpy Python ROS2
  nodes are now correctly classified on default Humble + Cyclone
  DDS hosts. Previously, every Python rclpy node was invisible to
  the classifier because:
  1. `rclpy.init()` does NOT export `ROS_DOMAIN_ID` — only `ros2
     launch` / `ros2 run` runners do. The env signal therefore
     fires for runner-spawned children but not for bare
     `python3 my_node.py` invocations.
  2. Typical `python3 my_node.py` cmdlines lack any ROS2 marker.
  3. `ROS2_LIBRARY_MARKERS` listed `librclpy.so` — a library that
     doesn't exist on Humble; rclpy is a Python package whose
     C-extension is `_rclpy_pybind11.cpython-<abi>-<arch>.so`.

  Fixed by replacing the fictional `librclpy.so` marker with three
  real, universally-loaded markers: `librcl.so` (the canonical
  underlying lib every ROS2 process loads, regardless of distro /
  RMW / language — closes DESIGN_HANDOFF.md L9 spec drift at lines
  128 / 335 / 1080), `librmw_implementation.so` (the RMW-discovery
  shim every ROS2 process loads), and `_rclpy_pybind11` (the
  C-extension Python rclpy actually links). Existing markers
  (`librclcpp.so`, `libfastdds.so`, `libfastrtps.so`) retained.

  Also corrected misleading module-level doc-comments at
  `src/classifier/ros2.rs` that claimed env vars were the "most
  reliable" signal (library linkage is — env vars don't fire for
  bare `rclpy.init()` / `rclcpp::init()` calls).

- **B-VRAM-ZERO (Tester-A + Inspector-A):** per-workload
  `peak_vram_mb` now reports correctly for AI workloads.
  Previously, every CUDA workload (Ollama, vLLM, llama.cpp,
  PyTorch, ROS2 perception with cuDNN) reported `peak_vram_mb=0`
  in RunStore records and `vram_mb=null` in live snapshots because
  `gpu_nvidia.rs::read_device_metrics` only called NVML's
  `running_graphics_processes()` API — which returns only OpenGL /
  Vulkan / X11 clients (compositor, browsers, games), never CUDA
  clients.

  Fixed by adding a parallel `running_compute_processes()` call
  that merges into the same per-PID VRAM map. Both NVML APIs are
  queried every tick; compute runs after graphics so a PID
  appearing on both lists keeps its compute reading. The merge
  step is extracted as a pure helper (`merge_per_process_vram`)
  so the runner-attribution invariant Tester-A confirmed
  empirically can be unit-tested without spinning up real NVML.

  Per Tester-A's confirmation
  (`tests/empirical/v1_0_3/b_vram_zero_confirmation/REPORT.md`):
  the allocating PID (ollama runner subprocess at PID 114547
  holding 838 MiB) appears in NVML's compute list directly; the
  daemon parent (ollama serve at PID 1685, 0 MiB) is invisible
  to NVML. No ppid reconciliation needed.

  Latent since 2026-04-28 (commit `d02a7eb0`). All v1.0.0–v1.0.2
  RunStore records on every host have been silently affected.
  Phase 3's governor pressure-detection depends on accurate
  per-PID VRAM; this fix is mandatory before Phase 3 work.

### Changed (hygiene)

- **RIDE 1:** removed stale "Governor will send real signals"
  startup WARN at `src/main.rs:173-177`. v1.0.1 flipped
  `default_ai_action` to Allow and routed the kill-verb branch in
  `record_governor_audit` to a no-op, so the warning claimed an
  automated kill path that no longer exists.
- **RIDE 2:** downgraded the RAPL `energy_uj` permission-denied
  log from `warn!` to `info!` at `src/telemetry/rapl.rs`. The
  once-per-process gate (`warned_unavailable`) was already in
  place; only the severity changed. INFO conveys the diagnostic
  without alarming operators on systems that lack
  `/sys/class/powercap` read perms by default.
- **RIDE 3:** fixed 4 `cargo doc` `unclosed HTML tag` warnings —
  `<pid>` / `<ProcessSample>` / `<Span>` in doc-comments at
  `src/model.rs:18-20` (two hits on the same struct), `src/
  platform/linux_proc.rs:57`, and `src/ui/panels/live_detail.rs:190`
  are now backtick-quoted so rustdoc no longer parses them as
  literal HTML.
- **RIDE 4 / B-NEW-20:** corrected the stale "NVML per-process
  memory tracking requires elevated privileges" comment at
  `src/platform/gpu_nvidia.rs:160`. No longer load-bearing on
  driver baseline ≥ 510; the new comment documents the
  graphics-vs-compute API split.

### Wire schema

Unchanged — still v0.1.

## [1.0.2] — 2026-05-22

Hotfix release closing two Inspector #5 classifier findings plus a
release-process hygiene gap from Tester's empirical sweep. No
wire-schema break.

### Fixed

- **B-NEW-16 — ROS2 introspection markers polluted workloads
  panel.** `ROS2_CMDLINE_MARKERS` in `src/classifier/ros2.rs`
  dropped three introspection-CLI substrings (`"ros2 topic"`,
  `"ros2 service"`, `"ros2 node"`). These are short-lived
  diagnostic shell-outs, not node-spawners. The operator's
  RunStore was carrying 55 transient 1-5 s `"ros2"` records
  that flooded both the Workloads panel and the Activity feed.
  Kept: `"ros2 run"`, `"ros2 launch"`,
  `"rclcpp_component_container"`, `"rclpy"`.
- **Sampler self-classification protection (Inspector #5).** Two
  guards added to `classify()`:
  1. `ROS2_TOOLING_NAMES` process-name blacklist
     (`rviz`/`rviz2`, `rqt*`, `colcon`, `ament_*`) — these
     processes link the rcl libraries but are tooling, not graph
     participants. Case-insensitive prefix match handles the
     kernel's `TASK_COMM_LEN=16` truncation of
     `/proc/<pid>/comm`.
  2. `bash -c "ros2 …"` / `sh -c "ros2 …"` shell-wrapper guard.
     Phase 2 will exec `bash -c "ros2 topic hz <topic>"` as its
     Hz sampler. Without this guard the sampler would classify
     its own probes as new ROS2 nodes, creating a feedback loop.
     The guard recognises bash/sh as `cmdline[0]` (basename
     match so `/bin/bash` works), then looks for `ros2` text in
     argv AFTER the `-c` separator.
- **B-EMPIRICAL-3 — release binary version hygiene.** Tester
  reported `target/release/edge_monitor --version` was still
  emitting `1.0.0` after the v1.0.1 tag because the release
  binary had not been rebuilt. Added `RELEASE.md` with the
  explicit `--release` rebuild + version-check step
  (`cargo pkgid` vs `--version` output) that every release must
  pass before tagging.

### Notes

- Test suite +11 vs the v1.0.1 baseline (9 new ROS2 classifier
  tests + 2 from carried-over module growth). Workspace total
  815 passed.
- No new contract dependency; v0.3.10 surface is unchanged.

## [1.0.1] — 2026-05-21

Hotfix release closing twelve Inspector-surfaced bugs found
post-`v1.0.0`. No new features and no wire-schema breaks; only
behavioural corrections and one additive wire field (`ram_pct`).

### Fixed

- **Phantom kills (B-NEW-1 + B-NEW-3).** `policy.default_ai_action`
  in `GovernorPolicy::safe_default()` flipped from `Kill` to `Allow`.
  v1.0.0 logged kill decisions to the audit trail without sending a
  real signal because `send_sigterm` was never wired up; the audit
  log read like the governor was killing AI workloads even though
  nothing died. The Allow-by-default flip closes the audit/reality
  gap. Operators who want automated kills must opt in via
  `edge_monitor.toml` (`[governor.policy] default_ai_action =
  "Kill"`) and accept that the executor itself still needs
  `send_sigterm` to be wired before any real kill fires. Surfaced
  in the help overlay and `edge_monitor.toml.example`.
- **Stale May-18 dry-run records (B-NEW-2).** RunRecord JSONs from
  pre-v1.0 ollama runs still carry `"Would stop ollama (dry-run
  mode — no action taken)"` in their `GovernorKill::reason`.
  Detect-and-tag at render time via the new
  `RunRecord::is_legacy_dry_run_record()` + `format_exit_short_for_record()`
  helpers; the raw JSON on disk is left intact (audit-trail
  preservation). Legacy rows now render as `governor (pre-v1.0
  dry-run mode — record retained for archeology)`.
- **Agent rows misclaiming activity (B-NEW-4 + B-NEW-6).** The
  primary-metric fallback for Agent workloads (claude-code, cursor,
  aider, continue) was `"running actively"` even when no metric was
  measured — Inspector #2 flagged this as a load-bearing lie. Agent
  rows now read `"alive"` (honest minimum signal: the process
  exists; no activity claim). LLM/Vision rows keep
  tokens/sec → fps → `"running actively"` precedence. Applied in
  both `src/ui/panels/workloads.rs` (`AGENT_ALIVE` const) and the
  web `WorkloadRow.svelte` per-category branch.
- **Activity feed flooded with shell exits (B-NEW-8).** The wire
  publisher (both TUI and headless paths) now filters recent
  RunRecords to `category.is_some()` before taking the last 50, so
  non-AI shell processes that briefly entered the lifecycle table
  don't crowd out the AI exits the operator actually wants to see.
- **`exit_kind = "unknown"` painting as a red alarm (B-NEW-11).**
  The web `ActivityFeed.svelte` `exitClass()` now routes `unknown`
  to `text-fg-muted` (no signal) and reserves `text-critical` for
  outcomes the runtime KNOWS went wrong (`crash`, `segfault`,
  `oom`, `cuda`). Exhaustive mapping with a muted-not-critical
  defensive default for future wire-schema variants.

### Added

- **`ram_pct` on `WireWorkload` (B-NEW-9, operator request).** RSS
  now renders as `121M (0.4%)` in the web dashboard when the
  platform layer surfaced a total. Falls back to bare megabytes
  when total is unknown — no misleading `0.0%`. Pure helper
  `compute_ram_pct(rss_mb, total_ram_bytes)` keeps the rule
  testable in isolation.
- **Help overlay governor disclosure (Inspector #1 rec).** `?`
  now states that the automated governor is DEFAULT DISABLED in
  v1.0.1 and shows the exact config snippet to opt back in.
  Manual kill (the `k` keybinding) is called out as unaffected.

### Contract Amendment Requests (deferred to a v0.3.10 batch on `ux_contract`)

These would have been single-line contract reads except that the
constants don't exist yet. Per the dispatch protocol (CLAUDE.md
"No UX changes without a contract amendment") this Linux PR keeps
the strings inline and surfaces the asks for Agent A to land in a
contract bump:

- **CAR — `status::RUNNING_ACTIVELY` + `status::AGENT_ALIVE`** (B-NEW-5).
  Both strings currently live as local `const`s in
  `src/ui/panels/workloads.rs`. The pair belongs in
  `ux_contract::status` so the sibling Windows binary picks up the
  same wording.
- **CAR — drop `status::KILL_DRY_RUN` + `status::KILL_DRY_RUN_PREFIX`**
  (B-NEW-7). Dry-run mode was hard-deleted from the runtime in
  Sprint 1 lead (`d8d7897`); these contract constants are orphans
  with no live caller in `src/`. Safe to remove in the next
  contract minor.
- **CAR — `activity_feed::MAX_RECENT_RECORDS`** (B-NEW-10). The cap
  of 50 lives inline in `runtime.recent_completed(50)` calls
  across the TUI and headless wire publishers. A contract const
  unifies the two and makes the cap a contract-versioned choice
  rather than an accident of two call sites happening to agree.

### Notes

- Wire schema stays at v0.1; the new `ram_pct` field is additive
  and serializes as `null` when unknown.
- `tests/governor_properties.proptest-regressions` now pins the
  `max=0, n_candidates=1` shrink that surfaced when FIX 1 first
  landed — checked in per proptest's recommendation so the
  regression always re-runs first.

## [1.0.0] — 2026-05-21

First stable Linux release. Real-time TUI dashboard for AI workloads,
embedded web companion with live WebSocket updates, kill-safety via
confirmation card overlay, post-mortem analysis with peak resource
tracking, Agent classification for SaaS-LLM developer-assistant CLIs.

Built on `ux_contract` v0.3.9 (CAR-17 `kill_confirm_card` +
CAR-18 `GROUP_HEADER_AGENT`).

The rest of this section preserves the per-sprint detail accumulated
under the prior `[Unreleased]` heading. A summary of the marquee
items follows.

### Added (v1.0.0 summary)

- **Web UI** — `axum` HTTP server + WebSocket live updates +
  Svelte reactive dashboard. Bundle embedded into the binary via
  `rust-embed`. Default bind `0.0.0.0:7070` (LAN-accessible);
  `--bind 127.0.0.1` to restrict to localhost on untrusted
  networks. (Sprint 6 + Sprint 7 Item 4).
- **Agent workload category** — claude / cursor / aider / continue
  now render under their own `── Agent ──` subsection in the
  Workloads panel (and `workload_category: "agent"` on the wire),
  separate from local LLM inference servers. Consumes
  `ux_contract::workload_category::GROUP_HEADER_AGENT` from
  v0.3.9. (Sprint 7.5).
- **kill_confirm card overlay** — CAR-17 replaced the pre-v1.0
  armed-banner kill pattern. `k` opens the card; `Enter` confirms
  the kill on the PINNED PID; `Esc` cancels. Card body shows
  workload identity, category, status, runtime, and live
  resource metrics so the operator decides with full context.
  (Sprint 1 lead).
- **Post-mortem card metrics** — Avg CPU, Peak CPU (Sprint 4),
  Peak RAM, Peak GPU memory, Throughput (LLM-only), Exit reason,
  baseline-status headline. Stderr-when-fresh tail (L19) when
  captured. (L16 + L19 + Sprint 4).
- **Mission-line wall clock** (Sprint 3 F1) — right-aligned
  `HH:MM:SS` local-timezone clock when terminal width allows;
  drops gracefully on narrow terminals.
- **Workload start-time column** (Sprint 3 F2 + Sprint 7 Item 3) —
  `HH:MM (Nm ago)` wide / `Nm ago` narrow. Reads
  `/proc/<pid>/stat` field 22 + `/proc/stat` `btime` for true OS
  spawn time; `first_observed_at` is a fallback only when `/proc`
  parse fails.
- **Workload Model column** (Sprint 3 F3 + Sprint 7 Item 2) —
  resolved model name from cmdline (`ollama run X`,
  `--model /path/Y.gguf`, `-m Z`). Ollama content-hash blobs
  (`sha256-XXX…`) get humanized to `sha256-XXXXXX…` so the
  column stays readable.

### Changed (v1.0.0 summary)

- **History overlay scope + columns** (Sprint 4 B13 + B14) —
  shows only completed/killed runs (RunStore is exit-only by
  construction); columns reduced to `# When  Dur  Exit`. Per-run
  metric detail (AvgCPU / PeakRSS / PeakVRAM / PeakCPU) moved
  into the post-mortem card body.
- **Workloads panel layout** (Sprint 4 layout fix) — Vitals,
  Workloads, Top processes, and Activity now have visible
  spacer rows between adjacent panel borders. Card overlays no
  longer make the borders read as "merged."
- **Footer keymap** (L25 + Sprint 1 lead) — `k kill (confirm)`,
  `j/k select`, `h history`, `? help`, `q quit`. Pre-v1.0 second
  `k` to fire is gone — `Enter` confirms instead, per CAR-17.

### Removed (v1.0.0 summary)

- **Dry-run mode** (Sprint 1 lead, d8d7897). The `kill_confirm`
  card IS the safety layer; dry-run no longer exists. `--dry-run`
  flag and `[policy].enforce` config field both removed.
- **Grafana integration** (Sprint 5). `g` keybinding, `[dashboard]`
  config, WP5 TCP preflight probe, and the `webbrowser` Cargo
  dependency are all gone. The v2 web companion (separate repo)
  handles the dashboard story.
- **Default allowlist over-reach** (Sprint 7 Item 1) —
  `is_allowlisted` previously delegated to a predicate that
  returned `Allow` for every process (showing every workload as
  `(ALLOWLISTED)` on the kill_confirm card). Fix: directly check
  the configured whitelist. Default whitelist remains as intended
  (`sshd / bash / zsh / sh / systemd / init / kworker / kthreadd`).

### Fixed (v1.0.0 summary)

- **Allowlist false-positive** (Sprint 7 Item 1).
- **sha256 blob shown as workload name** (Sprint 7 Item 2).
- **Spawn time reading "1m ago" for hours-old processes**
  (Sprint 7 Item 3 — resolves Sprint 3 F2 known limitation).
- **Vitals panel border merging with adjacent panel under any
  card overlay** (Sprint 4 layout fix).
- **Token/sec frozen on the workload row** (Sprint 2 investigation
  + B4 fixes — passive vLLM/llama.cpp scrape now reaches the
  workloads panel; ollama exec-wrapper covers the ollama path).
- **kk-kill PID drift under vitals refresh** (Row 1, post-CAR-17)
  — Enter confirms on the pinned PID, not on whatever
  `selected_pid` returns at the moment of the press.

### Known Limitations (v1.0.0)

- **New ollama spawn may not appear immediately on the dashboard**
  (Sprint 7 Item 5). Three hypotheses filed in BACKLOG.md
  "Open Sprint-7 follow-ups"; needs live reproduction with
  `RUST_LOG=debug`.
- **Web UI has no authentication.** Default bind is
  `0.0.0.0:7070` — trusted-LAN posture. Use `--bind 127.0.0.1`
  on untrusted networks. README "Web UI security" covers this.
- **Ollama passive tokens/sec unavailable.** Ollama embeds the
  per-request rate in JSON with no Prometheus endpoint; capture
  requires the `edge_monitor exec -- ollama …` wrapper path.
- **Windows binary on indefinite halt.** Linux is the v1.0
  reference implementation; Windows parity catches up post-v1.0.

### Contract dependency

- `ux_contract` v0.3.9 (path-dep). CAR-17
  (`kill_confirm_card` module) and CAR-18 (`GROUP_HEADER_AGENT`)
  are the v1.0-critical additions.

---

### Removed

- **Grafana integration hard-deleted in v1.0** (Sprint 5). The
  whole dashboard stack — `[dashboard]` config section,
  `DashboardConfig` struct, `g` keybinding (`Action::OpenGrafana`),
  `handle_open_dashboard` / `resolve_dashboard_template` /
  `compute_dashboard_url` / `format_grafana_unreachable` helpers,
  `src/dashboard_preflight.rs` (WP5 TCP probe),
  `tests/dashboard_keybinding_e2e.rs`, the `webbrowser` Cargo
  dependency, README + help-overlay mentions — is gone. The
  contract symbols `ux_contract::Action::OpenGrafana`,
  `ux_contract::status::GRAFANA_UNREACHABLE`,
  `ux_contract::status::DASHBOARD_OPENED`, and
  `ux_contract::status::DASHBOARD_FAILED` remain in the contract
  crate as orphans pending Agent A cleanup (a separate CAR). The
  `g` keybinding is now unbound. Existing user TOMLs with a
  `[dashboard]` section continue to load — serde ignores unknown
  sections under `Config`'s `#[serde(default)]`. Rationale: the
  integration was broken in practice, Grafana is a heavyweight
  dependency for the operator, and the v2 web companion (separate
  repo) handles the dashboard story.

### Added
- **Armed-kill banner** ([UX-1], `src/ui/panels/armed_banner.rs`).
  When a manual kill is armed (1st `k` press), a red full-width
  banner across the top of the TUI shows the target PID, name, and
  a 5-second auto-disarm countdown. Allowlisted targets render an
  `ALLOWLISTED, press k to override` variant. The banner replaces
  the previous inline status-bar marker (which was easy to miss).
  Window and string format locked by `UI_CONTRACT.md` (v2) for
  cross-platform parity.
- **Post-mortem card** ([UX-2],
  `src/ui/panels/postmortem.rs`). When an AI workload exits, a
  centered overlay surfaces the run summary — Duration, Avg CPU,
  Peak RAM, Peak GPU memory (omitted when zero), Throughput
  (omitted when no tokens/sec data), Exited — for 30 seconds, with
  a color-coded baseline headline below the field block (red
  `≥20% slower`, yellow `≥10% slower`, green `≥10% faster`, muted
  `matches baseline`, omitted entirely for first runs). `Esc` or
  `Enter` dismisses early. Only AI-classified exits trigger the
  card. Card title is the workload's `display_name`; width is
  fixed 64 columns; height computed from content and clamped to
  `[8, 22]` rows. The runtime builds a transient `PostMortem`
  struct at exit time and pushes a card through
  `Runtime::drain_postmortems`; **stderr is ephemeral** — the
  `PostMortem` carries `stderr_tail` and is dropped when the card
  is dismissed, never persisted to `RunRecord` (per UI Contract
  v2 schema decision).
- **`g` keybinding for dashboards** ([UX-3], UI Contract v2).
  Pressing `g` on a focused workload row opens a dashboard URL in
  the default browser, with `{model}` and `{pid}` substituted.
  URL source priority: `[dashboard].url_template` from config →
  `EDGE_MONITOR_GRAFANA_URL` env var → hardcoded
  `http://localhost:3000/d/edge_monitor` fallback. Refuses with a
  status hint when no row is focused. Uses the `webbrowser` crate
  (~50 KB stripped). Closes T2's V3 finding 1.
- **`[dashboard]` config section.** New `DashboardConfig` on
  `Config` with a single `url_template: String` field
  (default `""` = use env var or hardcoded fallback). Documented
  in `edge_monitor.toml.example` and `docs/configuration.md`.
  Static URLs (no `{model}` / `{pid}` tokens) are accepted.

### Changed
- **Cascading `Esc` priority in the TUI** (UI Contract v2).
  `App::handle_escape` routes Esc through: post-mortem card →
  armed-kill disarm → history overlay close → help overlay close
  → **quit (same as `q`)**. Each branch consumes the press and
  returns; the v2 fall-through to quit is new (v1 was a no-op when
  nothing was open). Filter mode still owns its own Esc.

### Notes
- `webbrowser` crate added to `Cargo.toml` (`default-features = false`,
  ~50 KB stripped). Binary remains over the 5 MB budget; size trim
  deferred per prior CHANGELOG note.
- UI Contract v2 reverts the v1 addition of
  `RunRecord.stderr_lines`. Stderr is now built ephemerally on a
  transient `PostMortem` struct at exit time and dropped when the
  card is dismissed. If a future feature needs stderr post-hoc
  (e.g. "show me what the last 3 failing runs printed"), that's a
  deliberate schema decision filed as a new feature, not a side
  effect of the post-mortem card.

- **Tier 3.4 history rendering** ([B-7], commit `e3a22da`,
  `src/history.rs`). `edge_monitor history MODEL` now renders the
  spec-example second line per row when concurrency telemetry is
  populated:
  `serving N concurrent (peak; M.M avg)  →  X.X tok/s/req · Y.Y
  tok/s aggregate[ · queue peak Q]`. Falls back to peak as the
  divisor when the time-weighted avg is missing (and names which
  divisor was used so a reader doesn't confuse the two), skips the
  per-request column on peak=0 to avoid Inf/NaN, and suppresses
  the queue-peak suffix when it's zero. Vision / Ollama /
  llama.cpp without busy-slot exposure render as before — the
  table stays compact for non-LLM history. `format_concurrent_line`
  is `pub(crate)` so future TUI overlays can reuse it. 6 new unit
  tests pin the matrix.
- **`[regression] baseline_strategy` + `drop_outliers` config**
  ([B-6], commit `25665b8`, `src/config.rs`+`src/runtime.rs`).
  Closes the wiring gap between `[C-5]`'s Mean/Median API and the
  toml example. `RegressionConfig` gains
  `baseline_strategy: String` (default `"mean"`) and
  `drop_outliers: bool` (default `false`). `validate()` rejects
  any strategy other than `"mean"`/`"median"` (case-insensitive)
  at load time with a message naming the offending value.
  `runtime::check_regressions` now reads `cfg.strategy()` /
  `cfg.drop_outliers` instead of the previously-hardcoded
  `Mean`/`false`, so `Baseline.strategy` and
  `Baseline.outlier_run_ids` reflect the user's choice.
  `edge_monitor.toml.example` and `docs/configuration.md` document
  both fields with their defaults.
- **Tier 3.4 — concurrent-request awareness**
  (`src/telemetry/concurrent_requests.rs`). New
  `TimeWeightedGauge` primitive folds `(value, instant)` samples into
  a step-function integral so we can answer "what was the typical
  concurrency" — distinct from the existing peak. The accumulator
  uses two gauges per PID (running + waiting) so a server that
  briefly touched 16 concurrent but spent most of its time at 2
  reports `peak=16, avg≈2`, not just `peak=16`. vLLM sampler now
  reads `vllm:num_requests_waiting` (queue depth, saturation
  signal). `RunMetrics` gains `concurrent_requests_avg: Option<f32>`
  (time-weighted) and `concurrent_requests_waiting_peak:
  Option<u32>`; existing `concurrent_requests_peak` semantics
  tighten — peak is `Some(value)` whenever any sample was observed,
  including peak=0, instead of collapsing peak=0 to None. Spec
  example "1 req for 10 s, 8 for 50 s" lands the textbook
  `(1·10 + 8·50)/60 ≈ 6.833` average. 7 unit tests cover the
  gauge edge cases (single sample, zero-Δt, all-zero values,
  backwards-time, 1000-sample precision); 3 integration tests in
  `tests/concurrent_requests_e2e.rs` cover the accumulator path.
  Smoke `scripts/manual/concurrent_requests_smoke.sh` runs the
  targeted tests and prints the spec calculation done two ways.

- **Tier 3.7 — `edge_monitor compare` CLI** (`src/compare.rs`, commit
  `0e3b518`). New subcommand `edge_monitor compare MODEL [MODEL ...]
  [--runs N] [--json]` folds the most recent N records per model into
  a Foundation-C `Baseline` and prints them in side-by-side columns
  (tok/s avg, peak VRAM, W/token, cold load). `--json` emits a
  `Vec<ComparisonColumn>` for piping into `jq`. W/token is computed as
  mean-of-ratios (per-record `energy_joules_total / tokens_total`,
  averaged across the window) so a single 1000-token run with 100 J
  doesn't outweigh five 100-token runs. Unknown models render an
  empty column rather than aborting — the operator wanted to see all
  requested models, including misses.
- **Tier 3.6 — vision probe Unix socket** (`src/telemetry/vision_probe.rs`,
  commit `a532928`). Listens on a Unix-domain stream socket for
  line-delimited JSON frame events (`{"pid": <u32>, "frame_at_ns":
  <u64>}`); each event aggregates into a per-PID rolling 1-second
  window and the instantaneous fps flows into the telemetry
  accumulator as a `TelemetryFrame`. Strict JSON, idle-disconnect
  timeout, malformed lines logged-and-dropped. Wired through
  `[telemetry] vision_probe_socket` (default empty = disabled).
- **Tier 3.5 — exit-reason classification** (`src/exit_classify.rs`,
  commit `95baf8b`). Layered classifier on top of
  `ExitReason::from_summary` that consults recent kernel-log lines
  (and, via Tier 1.2d exec, captured stderr) to distinguish OOM /
  Segfault / CudaError / Crash from a bare "signal X" answer. New
  `ExitReason` variants: `Segfault`, `OutOfMemory { ram, vram }`,
  `CudaError { last_msg }`. Precedence (highest first): governor →
  SIGSEGV → OOM (kernel via dmesg PID match, OR CUDA via stderr) →
  CUDA error → bare signal / exit code / Unknown. PID-misattribution
  guard: dmesg OOM lines match on `process <PID>` / `pid=<PID>`
  patterns ONLY, never on truncated process names.
  `read_recent_kernel_log(secs)` wraps `journalctl -k --since=-Ns`
  and returns `Vec::new()` on any failure so callers don't special-
  case host capability. `history::format_exit_short` gains compact
  tokens (`segfault`, `oom(ram)`, `oom(vram)`, `oom(ram+vram)`,
  `cuda_error`).
- **Tier 3.3 — KV cache pressure** (commit `83e299f`). `RunMetrics`
  gains `kv_cache_avg_pct` and `kv_cache_evictions_total`; the
  vLLM sampler scrapes `vllm:num_preemptions_total` for the
  evictions counter. Accumulator tracks per-PID KV avg via sum/count
  and evictions via first/last counter delta; out-of-range pct values
  are dropped, counter resets snap forward so the delta stays
  non-negative. TUI registry row appends a `KV NN%` segment, red+bold
  at ≥80%. History overlay tags runs whose peak hit ≥99.5% with a
  `KV!` badge so saturation events are visible at a glance.
- **Tier 3.2 — cold-start vs steady-state separation** (commit
  `47cb990`). Per-PID steady-state sub-aggregates activate when the
  Tier 2.2 cold-load detector declares the model load complete.
  `RunMetrics` adds `tokens_per_sec_avg_steady`, `fps_avg_steady`,
  `gpu_watts_avg_steady`. Frames recorded after the watermark
  contribute to BOTH overall totals AND the new `_steady` fields.
  `TelemetryAccumulator::mark_steady_state(pid)` flips the watermark;
  `Dispatcher::record_disk_io` calls it whenever
  `cold_load.record(pid, bytes)` returns `Some(stats)`.
- **Tier 3.1 — partial-hash model fingerprinting** (`src/fingerprint.rs`,
  commit `2ccbe73`). `fingerprint_model_file(path)` hashes
  `len_le_bytes || head[0..1MiB] || tail[len-64KiB..]` into SHA-256,
  prefixed `sha256-head1m-tail64k:` so the format is self-describing.
  Partial by design — a full hash of a 40 GB Llama-70B is too slow on
  every exit, head+tail differentiates quantization variants and
  distinct fine-tunes in <50 ms even on slow disks. Documented
  collision: middle-only changes share the same fingerprint, asserted
  by a test so future "fixes" surface deliberately. `Fingerprinter`
  caches results keyed on `(dev, inode, mtime_secs, len)` at the path
  configured by `storage.fingerprint_cache` (default
  `~/.cache/edge_monitor/fingerprints.json`); cache loaded on open,
  persisted on Drop or via explicit `persist()`. Malformed / wrong-
  version cache file silently resets. Runtime stamps the fingerprint
  onto `RunRecord.model_fingerprint` on every AI exit; cache hits
  avoid re-hashing on subsequent runs of the same weights file.
- **Tier 2.3 — Prometheus exporter** (`src/telemetry/exporter.rs`,
  commit `1f36487`). `GET /metrics` on `[telemetry] prometheus_bind`
  (e.g. `127.0.0.1:9472`) returns `text/plain; version=0.0.4`.
  Disabled by default (`prometheus_bind = ""`). Hand-rolled renderer
  — does not pull in the `prometheus` crate. Per-request 8 KiB header
  cap + 5 s read timeout protect against slowloris / memory-exhaust
  scrapes. Output sorted by label so golden-file diffing and Grafana
  caching are stable; NaN / Inf coerce to 0; backslash / quote /
  newline in labels escaped per spec. Metrics:
  `edge_monitor_processes_total{category}`,
  `edge_monitor_run_tokens_per_sec{model,pid}`,
  `edge_monitor_run_fps{model,pid}`,
  `edge_monitor_run_vram_bytes{model,pid}`,
  `edge_monitor_run_gpu_watts{model,pid}`,
  `edge_monitor_run_cpu_watts{model,pid}`,
  `edge_monitor_governor_kills_total{reason}`,
  `edge_monitor_regressions_total{model,metric}`,
  `edge_monitor_tick_count`. Snapshot is shared via
  `Arc<tokio::sync::Mutex>`; per-tick `try_lock`-fail drops the
  update so the tick loop never blocks on a long scrape.
- **Tier 2.2 — cold-load disk I/O detection** (`src/telemetry/cold_load.rs`,
  commit `cf73ead`). `ColdLoadTracker` watches `/proc/<pid>/io`
  `read_bytes` per AI process and declares cold-load complete when
  reads plateau after a sustained burst. Heuristic: 16 MiB floor +
  2 consecutive ≤1 MiB/s ticks ⇒ load complete. Hard timeout at 60 s
  for streaming inference workloads that never plateau — the tracker
  records what it has. Permission-denied / nonexistent PID returns
  `None` (both expected, neither error-worthy). `ColdStartStats`
  (`duration_seconds`, `bytes_read`, `avg_throughput_mbps`,
  `peak_throughput_mbps`) lands on `RunRecord.cold_start` on every AI
  exit. Per-PID state cleared via `forget(pid)` alongside the
  accumulator so recycled PIDs start fresh.
- **Tier 2.1 — NVML + RAPL power & thermals** (commit `0cc1b14`).
  `GpuDeviceMetrics` gains `power_watts: Option<f32>` and
  `temp_c: Option<f32>` from `nvmlDeviceGetPowerUsage` (mW → W) and
  `nvmlDeviceGetTemperature(GPU)`; NVML errors swallow into `None`
  rather than failing the whole per-device read. New `RaplReader`
  (`src/telemetry/rapl.rs`) discovers `/sys/class/powercap/intel-rapl:N`
  packages, holds last-energy/last-instant per package, and computes
  Δ-based wattage. Wraparound-safe via `max_energy_range_uj`.
  Permission-gated `energy_uj` (root-only on hardened distros) emits a
  single `tracing::warn!` then degrades to `None` watts. New
  `Dispatcher::record_system_power(processes, &GpuSnapshot)` runs each
  tick, sums GPU watts + max GPU temp + RAPL CPU watts, divides
  totals by AI-process count to apportion. `RunMetrics` carries
  `gpu_watts_avg`, `gpu_watts_peak`, `cpu_watts_avg`,
  `energy_joules_total` (trapezoidal integration of the wattage
  stream).
- **Tier 1.2d — `edge_monitor exec` wrapper subcommand**
  (`src/exec_wrapper.rs`, commit `4ba1bfc`; complements the earlier
  stdout regex parser). `edge_monitor exec [--name LABEL] -- COMMAND
  ARGS...` forks `COMMAND` with piped stdio, tees stdout/stderr to
  the invoking terminal AND through the `stdout_parser` sampler,
  aggregates per-line metrics into `ExecStats` (`tps_values`,
  `fps_values`, `latency_values`, plus a 64-line `stderr_tail` for
  exit classification), and on exit projects them onto `RunMetrics`
  (avg + peak tokens/sec, fps_avg, latency avg + p99 nearest-rank)
  and persists a `RunRecord`. SIGINT forwarding: Ctrl-C → child
  SIGINT; second Ctrl-C hard-exits 130 so a stuck child can't trap
  the user. Tier 3.5 hookup: stderr tail flows into `ExitContext` so
  CUDA OOM / CUDA error in the wrapped process classifies correctly.
- **Tier 1.2 dispatcher** (`src/telemetry/dispatcher.rs`). Closes the
  loop opened by Foundation B. Owns a 2-worker Tokio runtime, holds
  `Arc<Mutex<TelemetrySource>>` for each configured sampler, drives
  `applies_to + sample` against AI processes on every tick, drains
  resulting `TelemetryFrame`s through an unbounded mpsc channel into
  the per-PID `TelemetryAccumulator`, and enforces a per-sample
  timeout (default 1s) so a hung HTTP scrape can't pile up. Surfaces
  `metrics_for(pid)` and `model_name_hint_for(pid)` to the runtime.
- **Runtime → dispatcher wiring**. `Runtime::new` now constructs a
  dispatcher according to `[telemetry]` toggles and degrades
  gracefully when Tokio runtime construction fails. On every tick,
  AI-classified processes are pushed to the dispatcher; on every
  exit, accumulated metrics are merged onto the `RunRecord` AND the
  authoritative model_name (Tier 1.2c hint) is promoted onto the
  summary before the record routes to its per-model bucket. Per-PID
  state is forgotten after the record persists so recycled PIDs
  start fresh.
- **`[telemetry]` config section**: `vllm_scrape` (default true),
  `llamacpp_scrape` (default true), `ollama_api` (default true),
  `prometheus_bind` (empty disables — Tier 2.3 placeholder).
- 5 dispatcher unit tests: applicable source emits frames, non-
  applicable source's sample never called, slow sampler is timed
  out, panicking sampler does not bring down the runtime (other
  samplers continue), `forget(pid)` drops per-PID state.
- **Tier 1.2c — Ollama `/api/ps` sampler**

- **Tier 1.2b — llama.cpp server scraper**
  (`src/telemetry/samplers/llama_cpp_server.rs`). Detects `llama-server`
  on cmdline, scrapes `http://127.0.0.1:<port>/metrics` (default port
  8080). Reuses the `parse_metrics` Prom parser from 1.2a. When a
  direct tokens/sec gauge is missing, derives the rate from the
  monotonic `llama_server_n_decode_total` counter using a per-PID
  rolling `LastSample` (counter value + monotonic instant). Maps
  `llama_server_n_busy_slots` → `concurrent_requests`,
  `llama_server_kv_cache_usage` (0..1) → `kv_cache_pct` (×100).
- 9 unit tests including counter-delta rate derivation, idle-counter
  edge case (dn=0 → 0 tps), missing prior sample (None), and an
  end-to-end scrape against a tokio TcpListener.

- **Tier 1.2a — vLLM Prometheus scraper**
  (`src/telemetry/samplers/vllm_prometheus.rs`). Detects vLLM
  processes by cmdline (`vllm serve`, `vllm.entrypoints.*`,
  `python -m vllm`) or any `VLLM_*` env var, discovers the serving
  port from `--port N` / `--port=N` (default 8000), and scrapes
  `http://127.0.0.1:<port>/metrics` with a 500 ms timeout. Endpoint
  is cached per PID after first success. Maps standard vLLM metric
  names onto `TelemetryFrame`: `vllm:avg_generation_throughput_toks_per_s`
  → `tokens_per_sec`, `vllm:gpu_cache_usage_perc` → `kv_cache_pct`
  (×100 to convert 0..1 → %), `vllm:num_requests_running` →
  `concurrent_requests`. Parser is split from HTTP for offline
  unit-testability.
- New `reqwest = "0.12"` dependency (with `rustls-tls` + `http2`,
  default features off so no openssl).
- 10 unit tests including an end-to-end HTTP scrape against a tokio
  TcpListener serving canned bytes, plus exhaustive `parse_metrics`
  / `applies_to` / `discover_port` coverage.

- **Tier 1.2d — stdout regex parser**
  (`src/telemetry/samplers/stdout_parser.rs`). Pure-function
  `parse_line()` extracts `tokens_per_sec`, `fps`, and `latency_ms`
  from llama.cpp `llama_print_timings: eval time = ... tokens per
  second`, vLLM `Avg generation throughput: NN.N tokens/s`, and
  Ultralytics `Speed: Nms preprocess, Nms inference, Nms
  postprocess` log lines. Convenience `line_to_frame()` builds a
  `TelemetryFrame` ready for the accumulator. Strict parser — refuses
  partial matches so noise lines never produce 0.0 readings.
- New `regex` dependency (the std library has no equivalent and the
  patterns are too varied for hand-rolled parsing).
- 8 unit tests covering each runtime's log shape, fixture batch
  extraction, strict-mismatch invariant, and TelemetryFrame
  population.

- **Foundation B — telemetry sampler infrastructure** (latest.md).
  Defines the `TelemetrySource` async trait + `TelemetryFrame` +
  `TelemetryAccumulator`, plus error envelope (`SourceError::
  Transient | Permanent`). The accumulator folds repeated frames per
  PID into the `RunMetrics` shape `RunRecord` already carries, so
  Tier 1.2 samplers (vLLM / llama.cpp / Ollama / stdout) drop in
  without further plumbing. Concrete sources land in Tier 1.2.
- New `tokio` (with rt / time / sync / process / net features) and
  `async-trait` dependencies — pulled in for Foundation B; no
  blocking I/O leaks into the existing sync tick loop.
- 7 unit tests across `telemetry::source` and `telemetry::accumulator`
  (stub source returns frames, error variants round-trip, per-PID
  isolation, peak/avg arithmetic, p99 nearest-rank tail behaviour,
  NaN guard).

- **Tier 1.3 — regression warning on exit** (latest.md). When an
  AI-classified process exits, the runtime compares its `RunRecord`
  against the rolling baseline of prior runs (default window 10) and
  emits a `RegressionEvent` for each metric that exceeded the warn
  (10%) or critical (25%) threshold. Detection refuses to flag
  anything when the baseline has fewer than 3 samples, and the new
  record is excluded from its own baseline. Direction-aware: a
  higher `tokens_per_sec_avg` is never a regression; higher
  `peak_rss_mb` always is.
- New `[regression]` config section: `warn_pct`, `critical_pct`,
  `baseline_window`, `min_baseline_samples`. `config.validate()`
  rejects negative thresholds, critical < warn, zero window.
- TUI **Audit panel** retitled "Audit (kills + regressions)" and now
  interleaves kill entries and regression alerts by timestamp,
  newest first. Critical regressions render red, warnings yellow.
- Tracing emits one `tracing::warn!` per regression with structured
  fields (model, metric, baseline, current, delta_pct, severity) so
  headless and TUI users both see the alert.
- 4 unit tests in runtime.rs cover the exit hook: fires on metric
  blowup, silent on matching run, silent on tiny baseline, sink
  caps at the configured size.

- **Tier 1.1 — per-model run history viewer** (latest.md). Two surfaces:
  - **CLI subcommand** `edge_monitor history [MODEL] [--limit N] [--json]`.
    With no model, prints a table of (model, run count, last run,
    last status). With a model, prints the recent N runs with peak
    metrics. `--json` emits structured output (Vec<RunRecord> or
    Vec<ModelSummary>) for scripting.
  - **TUI overlay** triggered by `h` on a focused process row. Snapshots
    the most recent 20 runs of the row's model into a centered floating
    panel. Esc / q to close.
- **`[storage]` config section**: `run_store_path` (defaults to
  `~/.local/share/edge_monitor`), `fingerprint_cache`,
  `keep_runs_per_model`. Tilde expansion is built-in.
- **Runtime → RunStore wiring**: completed AI-classified runs are now
  persisted as `RunRecord`s into the typed store on every exit.
  Non-AI exits stay in the legacy `summary_log_path` JSONL when
  configured; RunStore is query-optimised (latest.md), not forensic.
- `Runtime::history(model, n)` accessor exposed for the TUI overlay.
- Manual smoke script: `scripts/manual/history_smoke.sh` drives the
  binary against a real yolo workload and checks both text and JSON
  shapes end-to-end.
- **Foundation A — `RunStore`** (`src/storage/run_store.rs`): typed
  read/write store for per-run records with a per-model index. Storage
  layout: `<root>/runs/<YYYY-MM-DD>/run-<uuid>.json` per record plus an
  append-only `index.jsonl` for fast startup scan. `RunRecord` embeds
  the existing `LifecycleSummary` and adds `run_id` (UUIDv4),
  `model_fingerprint`, `runtime`, `quantization`, `metrics: RunMetrics`,
  `exit_reason`, `cold_start`. API: `append`, `list_models`, `recent`,
  `get`, `baseline`. Crash-safe: record file is fsynced before the index
  entry is appended, so a partial write leaves an orphaned file (still
  recoverable) rather than a dangling index pointer.
- **Foundation C — baseline + regression detector**
  (`src/analysis/compare.rs`): per-metric mean/stddev baseline computed
  from a record slice, plus `detect_regressions(record, baseline)` that
  returns `Regression` entries above a configurable warn / critical
  threshold (defaults: 10% / 25%). Refuses to flag regressions when the
  baseline has fewer than 3 samples. Knows direction per metric — a
  faster `tokens_per_sec_avg` is never a regression.
- New `uuid` dependency (v1, `v4` + `serde` features).
- Manual smoke script: `scripts/manual/foundations_smoke.sh` runs the
  unit suites for both foundations.
- Phase 0 Linux build — all 8 modules complete.
  - Classifier: keyword matching with short-keyword word boundaries,
    cmdline/env model-path extraction, Python script sniffing, AI
    category assignment (Inference / Training / ModelDownload / Framework).
  - **Script-literal model extraction**: surfaces the actual weight file
    or repo id out of constructor calls like `YOLO("yolov8n.pt")`,
    `Llama(model_path="phi3-mini.gguf")`,
    `AutoModelForCausalLM.from_pretrained("meta-llama/Llama-3-8B")`, and
    `whisper.load_model("small.en")`.
  - Platform layer: `/proc` + `sysinfo` process sampling, CPU%, RSS,
    global network RX/TX deltas, graceful handling of permission-denied
    reads on `/proc/<pid>/environ`.
  - NVML GPU backend with per-process VRAM attribution where supported;
    returns `None` cleanly when no NVIDIA driver is present.
  - Lifecycle tracker: spawn/exit detection across snapshots, `RunSummary`
    generation on termination, PID-reuse safety.
  - **Resource accumulation**: per-process CPU-avg / CPU-peak / RSS-peak
    / VRAM-peak folded into the run summary every tick.
  - **Model name on run summaries**: `LifecycleSummary` carries the
    classifier's `model_name` so completed-process reports name the model
    rather than just the process.
  - Governor: allowlist-first policy, dry-run default, SIGTERM→grace→SIGKILL.
  - **Rate limit** (max 3 automated kills per 60-second sliding window)
    with a new `KillAction::RateLimited` variant and explicit tests for
    dry-run not consuming the budget and `max_kills = 0` meaning unlimited.
  - **Persistent audit trail**: `governor/audit.rs` writes one JSONL line
    per decision (manual + automated) to a configurable path; includes a
    `replay()` helper that tolerates torn tails.
  - **Persistent run-summary log**: `storage/log_store.rs` writes every
    `LifecycleSummary` to a separate JSONL file with round-trip tests.
  - Manual kill by selected PID in TUI; two-step `k` arm/confirm;
    allowlisted processes require explicit override confirm.
  - ratatui TUI with vitals / registry / rogues / culprits / completed /
    audit panels; 10 Hz render with cached data between 1 Hz ticks.
  - `main.rs` wiring: `clap` CLI (`--config`, `--dry-run`, `--no-ui`,
    `--ticks`, `--log-level`), TOML config loading, tracing-subscriber
    logging, clean Ctrl-C shutdown.
  - **Headless log**: one line per tick *plus* one line per AI process
    with pid, name, category, **model name**, CPU %, RSS MB, VRAM MB —
    so operators running without the TUI see the model, not just a count.
- Dual licensing under MIT OR Apache-2.0.

### Added
- **TUI detail-mode toggle (`v`).** The default view drops three
  panels — Framework procs, All processes, Recent actions — and
  shows just AI Workloads (full-width) plus Recent runs. Hit `v` to
  flip into detail mode and get the legacy six-panel layout back; hit
  `v` again to return. Tab focus-cycling is suppressed in default
  mode (only AI Workloads is on screen, so cycling would hide focus
  on a panel the operator can't see). Leaving detail mode snaps focus
  back to AI Workloads and disarms any pending manual kill — an
  armed kill against a row the user can no longer see is a footgun.
  The footer hint also updates per mode so the listed keys match the
  actions actually available. 4 new unit tests
  (`ui::app::tests::default_mode_locks_focus_to_registry`,
  `toggle_detail_mode_flips_the_flag_and_resets_focus`,
  `leaving_detail_mode_disarms_pending_kill`,
  `ui::input::tests::v_toggles_detail_mode`) plus
  `scripts/manual/detail_mode_smoke.sh` pin the wiring.

### Changed
- **UX pass — operator-facing labels rewritten in plain language.**
  TUI panel titles, headless log lines, and the governor's dry-run
  reason all dropped jargon-heavy phrasings:
  * `Registry (AI workloads)` → `AI Workloads`
  * `Rogues (unmapped framework procs)` → `Framework procs`
  * `Culprits (top by PID order)` → `All processes`
  * `Audit (kills + regressions)` → `Recent actions`
  * `AI run summaries` → `Recent runs`
  * `GPU: not available (NVML uninitialized)` → `No GPU detected`
  * `processes: N   AI-classified: M` → `N processes   M AI workloads detected`
  * Run-summary row now says `RAM 48 MB, GPU memory 4096 MB` instead
    of `rss=48M vram=4096M`, and drops the GPU memory clause entirely
    when the run had no GPU allocation.
  * `model=` is omitted (TUI and headless tracing) when no model name
    was extracted, instead of rendering `model=-` which read like a
    sentinel value. Same treatment for `vram=0M` / `peak_vram_mb=0`.
  * Governor dry-run reason `DRY-RUN: would send SIGTERM to AI
    process: Inference` → `Would stop ollama (dry-run mode — no
    action taken)`. Uses the actual process name and stops leaking
    the `AICategory` Debug variant.
  Verified by `scripts/manual/ux_rename_smoke.sh` (greps source for
  every old label, runs the binary headlessly to confirm the
  placeholder strings are gone) and a new
  `dry_run_reason_string_uses_process_name_and_plain_english`
  unit test in `governor::executor`.

### Fixed
- **V1 (S1) — Ollama tokens/sec now extracted via stdout parser.**
  Tester 2's V1 ground-truth check found that `edge_monitor`
  reported no `tokens_per_sec_avg` for any of three real
  `ollama run --verbose phi3` trials, even though Ollama itself
  printed `eval rate: 6.97 tokens/s` (etc) on stdout. Root cause:
  `stdout_parser.rs` had regexes for llama.cpp and vLLM tokens/sec
  output but no Ollama pattern, so the design's documented Tier 1.2c
  fallthrough ("Ollama tok/s falls through to stdout parsing")
  dead-ended. New regex
  `r"^\s*eval rate:\s+([0-9]+(?:\.[0-9]+)?)\s+tokens?/s\b"` matches
  Ollama's generation rate while explicitly NOT matching
  `prompt eval rate:` (a different, often higher number — trial 3
  had `prompt eval rate = 60.37` vs `eval rate = 2.34`). Verified
  against T2's captured trial outputs at `/tmp/v1_trial_{1,2,3}.out`
  by `scripts/manual/ollama_tps_smoke.sh`. Two new unit tests guard
  the fix: one asserts the three trial values parse correctly, one
  asserts the new regex does not poach existing vLLM / llama.cpp
  lines.

- **S.0.8 — SIGTERM clean shutdown re-verified and patched.** The
  audit flagged this as `needs re-verification — no commit message
  references the ctrlc termination feature`, and the audit was right
  — `kill -TERM <pid>` was bypassing the handler entirely (default
  kernel action, exit 143, no drain log, no audit flush). The
  `ctrlc` dependency now enables the `termination` feature, which
  routes SIGTERM and SIGHUP through the same atomic-flag handler
  SIGINT already used. After the fix: `edge_monitor --no-ui --ticks
  0` then `kill -TERM` exits 0, logs `shutdown requested; finishing
  current tick` and `shutdown signal received; exiting`, and leaves
  no orphan children. Smoke `scripts/manual/sigterm_smoke.sh` and
  integration test `tests/sigterm_clean_shutdown.rs` pin the
  behaviour.

### Added
- **S.2 — `--log-format json` flag**. Headless and exec runs accept
  `--log-format human` (default, K=V text — backwards-compatible)
  or `--log-format json` (one JSON object per stderr line, all
  structured fields flattened onto the root). Produced by
  `tracing_subscriber::fmt().json().flatten_event(true)` so
  downstream tooling (jq, fluentd, vector, python `json.loads`)
  can consume it without further parsing. Smoke
  (`scripts/manual/log_format_smoke.sh`) validates 100+ stderr
  lines parse as JSON; integration test
  (`tests/log_format.rs`) spawns the binary in both modes and
  asserts shape per format. Clap restricts the flag to those two
  values so a typo fails fast at parse time.

### Changed
- **S.3 — `expect()` rule reconciled with code.** CLAUDE.md's "no
  `expect()` outside tests" carve-out now lists three documented
  invariants (mutex-poison on critical writers, OnceLock-static
  `Regex::new`, and `reqwest::Client::builder().build()` in sampler
  constructors) and requires a `// ok: expect — <reason>` comment
  above every site. Every non-test `expect()` call in `src/` has
  been annotated; `scripts/manual/expect_audit.sh` enforces the rule
  and a Rust unit test guards it in CI.
- Tracing logs now route to **stderr** (was stdout) so subcommand JSON
  output (`history --json`) on stdout stays clean for piping into `jq`.
- **Release binary size grew from ~2.7 MB → ~7.4 MB** as a consequence
  of pulling in `tokio` (rt-multi-thread + time + sync + io-util +
  process + net) and `reqwest` (rustls-tls + http2). This puts the
  Linux binary over the spec's 5 MB budget; mitigation (cargo feature
  to disable HTTP samplers entirely; or switching to native-tls) is
  deferred — the launch-blocker is feature completeness, not size.

### Notes
- Developed on WSL Ubuntu; NVML returns `None` gracefully without GPU
  passthrough. Real target (Jetson AGX Orin) not yet validated end-to-end.
- 372 lib unit + 3 concurrent-request e2e + 8 dashboard-keybinding
  e2e + 1 expect-rule guard + 3 governor pid-reuse + 2 governor
  proptest + 5 history-CLI + 2 log-format + 3 pipeline + 9
  postmortem e2e + 1 SIGTERM clean-shutdown = 409 tests pass on
  release (`cargo test --release`).
- No release artifact yet. `v0.1.0` will be tagged once Phase 1 launch
  checklist (CI, demo GIF, `.deb`, crates.io name reservation) is complete.

[Unreleased]: https://github.com/Mohaaxa/edge_monitor/compare/HEAD...HEAD
