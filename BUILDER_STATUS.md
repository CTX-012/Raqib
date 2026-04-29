# Builder coordination state

This file is the single source of truth for builder activity.
Every builder must read this BEFORE starting work and update it
AFTER claiming a task.

Stale entries (>4 hours old) may be reclaimed.

Note: this repository has no git remote configured at the moment, so
"push" steps in the builder protocol are local-only commits. Builders
on the same machine still coordinate via this file (which is committed
to history) and via reading concurrent worktrees.

## Active Claims

- builder_id: builder-A
  scope: S.3 (expect rule), S.2 (--log-format flag), S.0.8 (SIGTERM
         re-verify), Tier 3.4 (concurrent-request awareness)
  branch: builder-claude/tier-3-3-kv-cache-pressure (continuing here
          because parallel-builder protocol allows it; new branch was
          not requested in this session's brief)
  started: 2026-04-29

- builder_id: builder-C
  scope: TEST.md gap closure
    - C-1 F.1.10 keep_runs_per_model prune logic + test (run_store)
    - C-2 F.1 1000-iter property test (run_store)
    - C-3 F.3.4 Warn-tier (12%) + boundary regression tests (compare)
    - C-4 F.1.7 ENOSPC disk-full test (run_store)
    - C-5 F.3.8 robust baseline median + outlier flag (compare)
  branch: builder-claude/tier-3-3-kv-cache-pressure
  started: 2026-04-29
  files: src/storage/run_store.rs, src/analysis/compare.rs,
         src/runtime.rs (run_store wiring only),
         src/exec_wrapper.rs (run_store wiring only),
         tests/, Cargo.toml (proptest dev-dep already present).
  cross-builder-request:
    - Builder B: edge_monitor.toml.example needs a [regression]
      baseline_strategy = "mean" example commented "or \"median\""
      once C-5 lands. I will not edit toml.example per the brief.

## Ready for Test

(populated as items land — see schema in builder brief)

## Cross-builder requests

- **Re: Builder A note about a failing prune test.** Builder C now
  owns and has resolved that path. `recent()` was sorting by append
  order, which is wrong once prune evicts mid-list; it now sorts by
  `summary.spawn_time` descending. The three new prune tests pass on
  release. Builder A's commits are unaffected.

## Recently completed (last 24h)

- builder_id: builder-claude
  feature: Tier 3.3 KV cache pressure
  branch: builder-claude/tier-3-3-kv-cache-pressure
  finished: 2026-04-28T10:48:00Z
  commits:
    - 83e299f feat(telemetry): KV cache pressure (latest.md Tier 3.3)
  notes: 309 unit tests + 13 integration tests pass; clippy clean.
         RunMetrics gained kv_cache_avg_pct + kv_cache_evictions_total.
         TUI registry row now shows "KV NN%" red at >=80%, history
         overlay flags runs with peak >=99.5% with a "KV!" badge.

## Locked files

(none — use this section for multi-file refactors)
