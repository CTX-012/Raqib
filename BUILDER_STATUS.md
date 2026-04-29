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

### [B-1] CHANGELOG.md backfill for Tier 1.2d exec, 2.1–2.3, 3.1–3.3, 3.5–3.7
- Commit SHA: 363769c5256633a4528916ebda2631b83ea46663
  (follow-up SHA `f0dfeb9` refreshes the test-count footer after
  Builder C's [C-1] prune fix landed; tester should review HEAD's
  CHANGELOG.md for the final wording)
- Files changed: CHANGELOG.md
- Verification command (what the tester should run):
  ```
  git show 363769c5256633a4528916ebda2631b83ea46663 -- CHANGELOG.md \
    | grep -cE '^\+- \*\*Tier (2\.[123]|3\.[12356]|3\.7|1\.2d)'
  ```
- Expected output (last 10 lines):
  ```
  10
  ```
- Builder note: ten new tier-line entries land under `[Unreleased] /
  Added`, each referencing the SHA that landed the corresponding work
  (`0cc1b14`, `cf73ead`, `1f36487`, `2ccbe73`, `47cb990`, `83e299f`,
  `95baf8b`, `a532928`, `0e3b518`, `4ba1bfc`).

### [B-2] FEATURES.md rewrite
- Commit SHA: 1a6454c9e70dd1d0507dcd8f410ee9624d69a2a4
  (follow-up SHA `f0dfeb9` refreshes the test-count paragraph after
  [C-1] landed)
- Files changed: FEATURES.md
- Verification command (what the tester should run):
  ```
  cargo test --release 2>&1 | grep -E '^test result' \
    | awk -F'[. ]+' '{passed += $4; failed += $6} END {print passed"/"(passed+failed)}'
  ```
- Expected output (last 10 lines):
  ```
  327/327
  ```
- Builder note: FEATURES.md's Test-Surface paragraph claims 313 lib
  unit + 1 expect-rule guard + 3 governor pid-reuse + 2 governor
  proptest + 5 history-CLI + 3 pipeline = 327 tests, all passing.
  Tier 3.4 is the only remaining feature gap called out under
  "Remaining gaps for v0.1.0"; everything else has moved out of
  "what this does not do".

### [B-3] edge_monitor.toml.example + docs/configuration.md [telemetry] section
- Commit SHA: 3d65df22e16f65835f2000044bfe583cb57fefdf
- Files changed: edge_monitor.toml.example, docs/configuration.md
- Verification command (what the tester should run):
  ```
  cargo build --release --quiet \
    && ./target/release/edge_monitor --config edge_monitor.toml.example \
        --no-ui --ticks 1 2>&1 | tail -4
  ```
- Expected output (last 10 lines):
  ```
  ... INFO loading config path=edge_monitor.toml.example
  ... INFO DRY-RUN mode — no signals will be sent.
  ... INFO tick tick=1 ai_processes=0 exits=0
  ... INFO tick budget reached; exiting ticks=1
  ```
- Builder note: each of the five `TelemetryConfig` fields is now
  documented in both the example TOML and `docs/configuration.md`
  with the default that `Default::default()` actually produces. No
  `[power]` section was added — see Cross-builder requests below.

## Cross-builder requests

- **Re: Builder A note about a failing prune test.** Builder C now
  owns and has resolved that path. `recent()` was sorting by append
  order, which is wrong once prune evicts mid-list; it now sorts by
  `summary.spawn_time` descending. The three new prune tests pass on
  release. Builder A's commits are unaffected.

- **From Builder B → auditor / Builder A: `[power]` config section
  is in latest.md but not in code.** latest.md "Cross-cutting
  requirements / Configuration additions" lists `[power]` with
  `rapl_enabled`, `nvml_power_enabled`, `tegrastats_enabled`. Today
  `src/config.rs` has no `PowerConfig` field on `Config` and the
  dispatcher unconditionally probes RAPL and NVML. Per the anti-
  celebration rule "no invented config defaults," Builder B did not
  add the section to `edge_monitor.toml.example` or
  `docs/configuration.md`. `docs/configuration.md` documents the
  absence in a trailing note under `[telemetry]`. Decision needed:
  either implement `PowerConfig` so the example matches the spec, or
  amend latest.md so the spec matches the code.

- **From Builder B → Builder C: pending C-5 baseline_strategy
  example.** Acknowledged. Once C-5 lands `baseline_strategy =
  "mean"` / `"median"`, Builder B will append it to
  `edge_monitor.toml.example` and `docs/configuration.md` in a
  follow-up `[B-?]` claim.

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
