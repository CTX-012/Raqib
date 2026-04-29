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
  scope: ALL DONE — S.3 → [A-1], S.2 → [A-2], S.0.8 → [A-3],
         Tier 3.4 → [A-4] in Ready for Test.
  branch: audit/2026-04-29 (renamed mid-session; all four A-*
          commits land here).
  finished: 2026-04-29

- builder_id: builder-C
  scope: TEST.md gap closure — ALL DONE, see [C-1] through [C-5]
         in Ready for Test.
  branch: audit/2026-04-29 (was builder-claude/tier-3-3-kv-cache-
          pressure; the working tree was renamed mid-session by
          something in the harness, all five C-* commits are on
          the renamed branch).
  finished: 2026-04-29

## Ready for Test

### [A-1] S.3 — `expect()` rule reconciled with code
- Commit SHA: c9fe87c1a0889d1c28139840ad749659634ce9b1
- Files changed: CLAUDE.md, CHANGELOG.md, src/storage/log_store.rs,
  src/telemetry/samplers/vllm_prometheus.rs,
  src/telemetry/samplers/llama_cpp_server.rs,
  src/telemetry/samplers/ollama_api.rs,
  scripts/manual/expect_audit.sh (new),
  tests/expect_rule_guard.rs (new)
- Smoke script: scripts/manual/expect_audit.sh
- CHANGELOG line: "**S.3 — `expect()` rule reconciled with code.**
  CLAUDE.md's \"no `expect()` outside tests\" carve-out now lists
  three documented invariants (mutex-poison on critical writers,
  OnceLock-static `Regex::new`, and
  `reqwest::Client::builder().build()` in sampler constructors) and
  requires a `// ok: expect — <reason>` comment above every site.
  Every non-test `expect()` call in `src/` has been annotated;
  `scripts/manual/expect_audit.sh` enforces the rule and a Rust unit
  test guards it in CI."
- Test output (selected — full run shows 1 unrelated proptest failure
  filed in Cross-builder requests below):
  ```
  test storage::log_store::tests::ok ... ok
  test telemetry::samplers::vllm_prometheus::tests::compute_frame_extracts_kv_avg_and_evictions ... ok
  test telemetry::samplers::llama_cpp_server::tests::applies_to_recognises_llama_server ... ok
  test telemetry::samplers::ollama_api::tests::applies_to_recognises_ollama_serve ... ok
  ---
  Running tests/expect_rule_guard.rs
  test every_prod_expect_is_annotated ... ok
  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured
  ---
  scripts/manual/expect_audit.sh:
  PASS: 34 expect() sites scanned (annotated PROD or test); none violate the rule.
  ```
- Builder note: the audit's "34 expect() sites in non-test code"
  number was loose; only 11 of the 34 actually sit outside
  `#[cfg(test)]` (the rest are inside a `#[cfg(test)] mod tests`
  block at the bottom of each source file, which the rule has
  always allowed). All 11 PROD sites now have `// ok: expect —
  <reason>` comments and the rule has been broadened to three
  named patterns; the `tests/expect_rule_guard.rs` integration test
  was confirmed to fail when a deliberate violator file was dropped
  into `src/` (then deleted, all green again). The test's runtime
  is sub-millisecond because the repo is small (~50 .rs files), but
  it does walk every file — the deliberate-violation check verified
  it isn't a stub. No production behaviour changed.

### [A-2] S.2 — `--log-format json` flag
- Commit SHA: 84e413390df7f24587e05d143c122e7543fb377a
- Files changed: CHANGELOG.md, scripts/manual/log_format_smoke.sh
  (new), tests/log_format.rs (new). Note: the clap field and
  `init_tracing` JSON branch on `src/main.rs` were already in place
  from a prior session; this commit closes the test + smoke +
  CHANGELOG gap the audit called out.
- Smoke script: scripts/manual/log_format_smoke.sh
- CHANGELOG line: "**S.2 — `--log-format json` flag**. Headless and
  exec runs accept `--log-format human` (default, K=V text —
  backwards-compatible) or `--log-format json` (one JSON object per
  stderr line, all structured fields flattened onto the root).
  Produced by `tracing_subscriber::fmt().json().flatten_event(true)`
  so downstream tooling (jq, fluentd, vector, python `json.loads`)
  can consume it without further parsing. Smoke
  (`scripts/manual/log_format_smoke.sh`) validates 100+ stderr lines
  parse as JSON; integration test (`tests/log_format.rs`) spawns the
  binary in both modes and asserts shape per format. Clap restricts
  the flag to those two values so a typo fails fast at parse time."
- Test output (last 10 lines of `cargo test --release`):
  ```
  test json_format_emits_one_json_object_per_stderr_line ... ok
  test human_format_is_not_json_shaped ... ok
  test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured
  Running tests/pipeline_end_to_end.rs
  test ai_process_with_model_path_is_tracked_and_killed_in_enforce_mode ... ok
  test exited_ai_process_generates_summary_with_resource_stats ... ok
  test persistent_summary_round_trips_through_log_store ... ok
  test result: ok. 3 passed; 0 failed
  Doc-tests edge_monitor
  test result: ok. 0 passed; 0 failed
  ```
- Builder note: smoke output on my WSL box was "PASS: --log-format
  json is jq-clean over 100+ lines; human format remains text." with
  103 stderr lines captured. The smoke writes a tempdir config that
  sets `tick_interval_ms = 100` so 100 ticks complete in ~10 s
  rather than 100 s. Tester 1 should re-run the smoke on a real
  Linux box; Tester 2 should re-run `cargo test --release --test
  log_format` and confirm both subprocess tests pass.

### [A-3] S.0.8 — SIGTERM clean shutdown re-verified and patched
- Commit SHA: e4a7e7455e864302d42c11259888de2f439dbeef
- Files changed: Cargo.toml, Cargo.lock, src/main.rs (comment only on
  install_shutdown_handler), CHANGELOG.md,
  scripts/manual/sigterm_smoke.sh (new),
  tests/sigterm_clean_shutdown.rs (new)
- Smoke script: scripts/manual/sigterm_smoke.sh
- CHANGELOG line: "**S.0.8 — SIGTERM clean shutdown re-verified and
  patched.** The audit flagged this as `needs re-verification — no
  commit message references the ctrlc termination feature`, and the
  audit was right — `kill -TERM <pid>` was bypassing the handler
  entirely (default kernel action, exit 143, no drain log, no audit
  flush). The `ctrlc` dependency now enables the `termination`
  feature, which routes SIGTERM and SIGHUP through the same
  atomic-flag handler SIGINT already used. After the fix:
  `edge_monitor --no-ui --ticks 0` then `kill -TERM` exits 0, logs
  `shutdown requested; finishing current tick` and `shutdown signal
  received; exiting`, and leaves no orphan children. Smoke
  `scripts/manual/sigterm_smoke.sh` and integration test
  `tests/sigterm_clean_shutdown.rs` pin the behaviour."
- Test output (last 10 lines of `cargo test --release --test
  sigterm_clean_shutdown`):
  ```
  Compiling edge_monitor v0.1.0 (/home/faisal/edge_monitor)
  Finished `release` profile [optimized] target(s)
  Running tests/sigterm_clean_shutdown.rs
  running 1 test
  test sigterm_drains_and_exits_zero ... ok
  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;
                  0 filtered out; finished in 0.56s
  ```
- Builder note: the BEFORE/AFTER reproducer in the commit message
  shows the exit-code change from 143 → 0 directly. Tester 1 should
  re-run the smoke on real bare-metal Linux (and ideally on Jetson
  Orin) to confirm the same behaviour off WSL. Tester 2 should
  re-run `cargo test --release --test sigterm_clean_shutdown` and
  verify the assertion that catches a regressed `termination`
  feature (the test panic message walks the reader through what to
  check). The smoke also catches orphan children via `ps --ppid`,
  which the integration test deliberately leaves to shell.

  Pre-existing failure
  `storage::run_store::tests::append_returns_err_when_filesystem_rejects_write`
  is currently red on this branch — that's a Builder C territory
  bug (chmod-readonly check fails when the test runs as a user
  whose effective UID can still write to readonly dirs, e.g. inside
  some WSL / container setups). Not caused by this commit; flagged
  in Cross-builder requests below.

### [A-4] Tier 3.4 — concurrent-request awareness
- Commit SHA: 959c477b53e62cc6c732b35f1ac1f28f37a6b4bd
- Files changed: CHANGELOG.md, src/telemetry/concurrent_requests.rs
  (new module), src/telemetry/mod.rs (export), src/telemetry/source.rs
  (added `num_requests_waiting` to TelemetryFrame),
  src/telemetry/samplers/vllm_prometheus.rs (parse
  `vllm:num_requests_waiting`), src/telemetry/accumulator.rs
  (replaced `concurrent_peak: u32` with two TimeWeightedGauges),
  tests/concurrent_requests_e2e.rs (new),
  scripts/manual/concurrent_requests_smoke.sh (new). The
  `RunMetrics::concurrent_requests_avg` and `_waiting_peak` fields
  on `src/storage/run_store.rs` were authored by [A-4] but landed
  via [C-4]'s commit (Builder C absorbed an uncommitted edit when
  they staged run_store changes in a parallel worktree). Net effect
  is identical and Tier 3.4's tests cover the behaviour.
- Smoke script: scripts/manual/concurrent_requests_smoke.sh
- CHANGELOG line: "**Tier 3.4 — concurrent-request awareness**
  (`src/telemetry/concurrent_requests.rs`). New `TimeWeightedGauge`
  primitive folds `(value, instant)` samples into a step-function
  integral so we can answer \"what was the typical concurrency\" —
  distinct from the existing peak. The accumulator uses two gauges
  per PID (running + waiting) so a server that briefly touched 16
  concurrent but spent most of its time at 2 reports `peak=16,
  avg≈2`, not just `peak=16`. vLLM sampler now reads
  `vllm:num_requests_waiting` (queue depth, saturation signal).
  `RunMetrics` gains `concurrent_requests_avg: Option<f32>`
  (time-weighted) and `concurrent_requests_waiting_peak:
  Option<u32>`; existing `concurrent_requests_peak` semantics
  tighten — peak is `Some(value)` whenever any sample was observed,
  including peak=0, instead of collapsing peak=0 to None. Spec
  example \"1 req for 10 s, 8 for 50 s\" lands the textbook
  `(1·10 + 8·50)/60 ≈ 6.833` average. 7 unit tests cover the gauge
  edge cases (single sample, zero-Δt, all-zero values,
  backwards-time, 1000-sample precision); 3 integration tests in
  `tests/concurrent_requests_e2e.rs` cover the accumulator path.
  Smoke `scripts/manual/concurrent_requests_smoke.sh` runs the
  targeted tests and prints the spec calculation done two ways."
- Test output (last 10 lines of `cargo test --release`):
  ```
  test result: ok. 324 passed; 0 failed; 0 ignored; 0 measured
  test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured (compare proptests)
  test result: ok. 3 passed; 0 failed (concurrent_requests_e2e — NEW)
  test result: ok. 1 passed; 0 failed (expect_rule_guard — A-1)
  test result: ok. 3 passed; 0 failed (governor_pid_reuse)
  test result: ok. 2 passed; 0 failed (governor_properties)
  test result: ok. 5 passed; 0 failed (history_cli)
  test result: ok. 2 passed; 0 failed (log_format — A-2)
  test result: ok. 3 passed; 0 failed (pipeline_end_to_end)
  test result: ok. 1 passed; 0 failed (sigterm_clean_shutdown — A-3)
  ```
- Builder note:
  * **Spec interpretation is documented at the top of
    `src/telemetry/concurrent_requests.rs`** (lines 1–56). The
    brief said "if the spec is ambiguous, write down your
    interpretation as a comment block at the top of the new module";
    latest.md's "track both peaks and time-weighted averages" left
    open whether single-sample runs should report the lone value as
    the average. I picked **None** for those — averaging over zero
    elapsed time is undefined, and surfacing the value as both peak
    and avg would let it double-count against later samples that
    arrive after a snapshot. Same call applies when every sample
    arrived at the same `Instant` (zero Δt). Peak still reports in
    those cases (it's a separate statistic with no time dimension).
  * **`concurrent_requests_peak` semantic shift.** Previously the
    accumulator surfaced `None` when the running peak landed on
    exactly 0 (an idle vLLM server polled at the wrong moment). Now
    it surfaces `Some(0)` because we *did* observe data. Distinct
    from `None` (no data ever arrived for this PID). The old shape
    was lossy and no test pinned it.
  * **Did NOT extend the history viewer.** latest.md's example
    output `#14  serving 8 concurrent (peak)  →  20.1 tok/s/req`
    is a UI rendering concern that touches `src/history.rs` —
    `history.rs` is on Builder B's surface for a parallel
    polishing pass and rendering arithmetic is a separate
    concern from the data path I shipped. Cross-builder request
    filed below.

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

### [C-1] F.1.10 — `keep_runs_per_model` prune logic on `RunStore::append`
- Commit SHA: 5c3ec5cc734c08b9885fb0f3f60152fb49dabaf9
- Files changed: src/storage/run_store.rs, src/runtime.rs (one-line
  wiring of config.storage.keep_runs_per_model into the opened store),
  src/exec_wrapper.rs (same one-line wiring), BUILDER_STATUS.md
- New tests added:
    - storage::run_store::tests::prune_keeps_three_newest_by_spawn_time
    - storage::run_store::tests::prune_with_limit_two_leaves_two_files_on_disk
    - storage::run_store::tests::pruned_ids_stay_pruned_after_reopen
- Test output proving the new tests ran:
  ```
  $ cargo test --release --lib storage::run_store::tests::prune
  Finished `release` profile [optimized] target(s) in 0.15s
       Running unittests src/lib.rs (target/release/deps/edge_monitor-6cd854fb5988a928)

  running 3 tests
  test storage::run_store::tests::prune_keeps_three_newest_by_spawn_time ... ok
  test storage::run_store::tests::pruned_ids_stay_pruned_after_reopen ... ok
  test storage::run_store::tests::prune_with_limit_two_leaves_two_files_on_disk ... ok

  test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 321 filtered out; finished in 0.00s
  ```
- Builder note: the spec required spawn_time-driven eviction, so
  `recent()` now sorts loaded records by `summary.spawn_time` desc.
  That fixed the pre-existing failing-test report Builder A filed
  before claiming [A-1]. Tombstones in `index.jsonl` keep pruned
  ids out across reopens (otherwise the in-memory `by_model` would
  re-include every id). No breaking change to Baseline / RunRecord
  serde format. Manual scenario in
  `prune_with_limit_two_leaves_two_files_on_disk` confirms 10
  appends with limit=2 leave exactly 2 record files on disk.

### [C-2] F.1 — 1000-iteration property test for `RunStore`
- Commit SHA: 78697712e7ce2128d2835b391d76b84adeaeaa78
- Files changed: src/storage/run_store.rs (new `prop_tests` module),
  proptest-regressions/storage/run_store.txt (added in [C-4] commit
  along with the ENOSPC test — proptest auto-generated seed for
  the C-2 sort-bug verification, kept as a permanent regression
  pin)
- New tests added:
    - storage::run_store::prop_tests::append_recent_invariants
- Test output proving the new test ran:
  ```
  $ cargo test --release --lib storage::run_store::prop_tests::append_recent_invariants -- --nocapture
  Finished `release` profile [optimized] target(s) in 0.13s
       Running unittests src/lib.rs (target/release/deps/edge_monitor-6cd854fb5988a928)

  running 1 test
  proptest::append_recent_invariants passed 1000 cases
  test storage::run_store::prop_tests::append_recent_invariants ... ok

  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 323 filtered out; finished in 0.68s
  ```
- For F.1: the line proving 1000 cases ran:
  ```
  proptest::append_recent_invariants passed 1000 cases
  ```
- Builder note: the line is emitted by an `eprintln!` from inside
  the proptest body on the final case (counted via a static
  `AtomicUsize`), so a future shrink of the configured `cases`
  value would silently stop emitting it — that's the failure mode
  the brief warned about ("sub-millisecond runtime means the test
  isn't doing what you think"). Wallclock for 1000 cases on this
  WSL box is ~0.5–0.7 s — about 500–700 µs per case, plausible for
  tempdir + a few file writes on tmpfs. Anti-celebration check:
  deliberately commented out the `recent()` sort and confirmed the
  proptest fails with a shrunk 2-append-+-Recent counterexample;
  restored. The shrunk seed is now in
  `proptest-regressions/storage/run_store.txt` and re-runs first on
  every future invocation.

### [C-3] F.3.4 — Warn-tier (12%) + boundary regression matrix
- Commit SHA: 3ec02724ffa827ded3948b3139571eed43e4dcad
- Files changed: src/analysis/compare.rs (tests only)
- New tests added:
    - analysis::compare::tests::twelve_percent_drop_is_warn_not_critical
    - analysis::compare::tests::warn_critical_boundary_matrix
- Test output proving the new tests ran:
  ```
  $ cargo test --release --lib analysis::compare
  Finished `release` profile [optimized] target(s) in 0.13s
       Running unittests src/lib.rs (target/release/deps/edge_monitor-6cd854fb5988a928)

  running 9 tests
  test analysis::compare::tests::higher_rss_is_a_regression ... ok
  test analysis::compare::tests::baseline_metrics_per_metric_n ... ok
  test analysis::compare::tests::faster_run_is_not_a_regression ... ok
  test analysis::compare::tests::matching_record_no_regressions ... ok
  test analysis::compare::tests::slow_run_is_critical ... ok
  test analysis::compare::tests::tiny_baseline_emits_no_regressions ... ok
  test analysis::compare::tests::twelve_percent_drop_is_warn_not_critical ... ok
  test analysis::compare::tests::warn_critical_boundary_matrix ... ok
  ```
- Builder note: all five boundary cases (9.99 / 10.01 / 12 / 19.99
  / 20.01 percent slowdown) run as part of
  `warn_critical_boundary_matrix`. The 19.99 / 20.01 split needs
  `RegressionConfig { critical_pct: 20.0, .. }` to land on the
  intended side of the threshold (defaults are warn=10 / crit=25);
  the 12% mid-band still uses defaults. Anti-celebration: changed
  `if delta_pct < cfg.warn_pct` to `cfg.critical_pct` in the
  comparator and confirmed the boundary test reports
  `10.01% (just above warn): expected regression, got none` —
  exactly the kind of message a reviewer can act on.

### [C-4] F.1.7 — write-rejection / disk-full path on `append`
- Commit SHA: c6a832ac8363d73d1ff5ba36266c2a80e3eb264e
  (follow-up SHA in HEAD adds a runtime probe that skips the test
  cleanly on filesystems where chmod doesn't actually deny writes
  — addresses Builder A's flag in Cross-builder requests below)
- Files changed: src/storage/run_store.rs,
  proptest-regressions/storage/run_store.txt (new — proptest
  auto-saved C-2 shrink seed)
- New tests added:
    - storage::run_store::tests::append_returns_err_when_filesystem_rejects_write
- Test output proving the new test ran:
  ```
  $ cargo test --release --lib storage::run_store::tests::append_returns_err
  Finished `release` profile [optimized] target(s) in 0.10s
       Running unittests src/lib.rs (target/release/deps/edge_monitor-6cd854fb5988a928)

  running 1 test
  test storage::run_store::tests::append_returns_err_when_filesystem_rejects_write ... ok

  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 323 filtered out; finished in 0.00s
  ```
- Builder note: the test mocks ENOSPC at the cheapest portable
  boundary — chmod 0o555 on the per-day record dir, so the next
  `OpenOptions::create_new` returns EACCES. The `RunStoreError::
  WriteRecord { source: io::Error, path }` codepath under test is
  the same one ENOSPC would hit, so the contract (Err-not-panic,
  message names the failing path, in-memory state stays
  consistent) is identical. Mock approach is documented in the
  test's doc comment per the brief. Re: Builder A's WSL/overlayfs
  flag — the follow-up commit probes write access *after* the
  chmod and skips the test (with a clear `eprintln!` reason)
  rather than asserting on environments where chmod metadata
  doesn't actually deny writes. Anti-celebration: deliberately
  made `append` swallow the open error and pretend success;
  confirmed the test fails with `"append should fail when the day
  dir is read-only: <uuid>"`; restored.

### [C-5] F.3.8 — Robust baseline (median + outlier flag)
- Commit SHA: b4ddd150e512d0c72694dea0d7a4c8882c0f0ef5
- Files changed: src/analysis/compare.rs (impl + test),
  src/storage/run_store.rs, src/runtime.rs, src/compare.rs (the
  three Baseline-literal call sites; threading the strategy and
  outlier list through, defaults preserved)
- New tests added:
    - analysis::compare::tests::robust_baseline_median_unaffected_by_outlier
- Test output proving the new test ran:
  ```
  $ cargo test --release --lib analysis::compare::tests::robust_baseline
  Finished `release` profile [optimized] target(s) in 0.12s
       Running unittests src/lib.rs (target/release/deps/edge_monitor-6cd854fb5988a928)

  running 1 test
  test analysis::compare::tests::robust_baseline_median_unaffected_by_outlier ... ok

  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 323 filtered out; finished in 0.00s
  ```
- Builder note: API is opt-in —
  `BaselineMetrics::from_records(records)` still uses Mean +
  outliers-included for source compatibility; the new behaviour
  comes through `from_records_with(records, strategy,
  drop_outliers)`. Outlier flag rule is fixed to the brief's
  "> 2 stddev from the median" and is independent of the chosen
  strategy — both branches of the test get the same outlier set,
  only the centre moves. Baseline gained `outlier_run_ids` and
  `strategy` with `serde(default)` so older serialised baselines
  still parse. Cross-builder request to Builder B for an
  `edge_monitor.toml.example` line is in the section below — I
  did not edit toml.example per the brief.

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

- **From Builder A → Builder B: history viewer should show the new
  Tier 3.4 numbers.** latest.md's spec example for Tier 3.4 calls
  out a per-row history rendering:
  `#14  serving 8 concurrent (peak)  →  20.1 tok/s/req · 161 tok/s
  aggregate`. The data layer ([A-4]) now lands
  `concurrent_requests_avg`, `_peak`, and `_waiting_peak` on
  `RunMetrics`, so a tester or downstream consumer can read the
  numbers via `history --json` today. Adding the per-row text
  rendering touches `src/history.rs` — Builder B's polishing
  surface — and the arithmetic for "tok/s/req" is `tps_avg /
  concurrent_avg` (guard concurrent_avg > 0). I did not edit
  history.rs per the brief's "do not edit other builders' files"
  rule; please pick this up in a follow-up B-? claim, or push back
  if you'd rather Tier 3.4 own the rendering layer too.

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

- **From Builder A → Builder C: new lib test failing as I write
  this.** `cargo test --release` HEAD shows `storage::run_store::
  prop_tests::zzz_proptest_actually_ran_1000_cases` panicking with
  "proptest executed only 0 cases; expected >= 1000". This is C-2
  WIP and will need a proptest config fix (probably a missing
  `ProptestConfig::cases(1000)` or a strategy that prunes itself
  to zero). Not blocking [A-1]; flagged here so the auditor
  attributes the failure correctly. **Update**: appears to have
  been resolved by Builder C between when I noticed it and when I
  finished [A-2] — the test is no longer in the failure list.

- **From Builder A → Builder C (round 2): `storage::run_store::
  tests::append_returns_err_when_filesystem_rejects_write` flips
  red on my WSL setup.** The fixture chmods a tempdir 0o555 and
  expects `append` to fail with EACCES, but on this box (WSL Ubuntu,
  ext4 mount over Windows) the chmod doesn't actually deny writes,
  so `append` succeeds and the test panics. The test is a real
  positive on bare-metal Linux but flaky on WSL / overlayfs /
  Docker-rootless. Builder C may want to gate it on
  `nix::sys::stat::access(F_OK | W_OK)` after the chmod — if the
  process can still write the dir, skip the test with `eprintln!`
  rather than asserting. Not blocking [A-3]; my test for S.0.8
  passes regardless.

  **Resolved by Builder C** (HEAD of `audit/2026-04-29`): the
  test now writes a sentinel `.write-probe` file inside the
  chmodded day-dir before asserting; if the write succeeds (CAP_
  DAC_OVERRIDE, overlayfs that ignores chmod, rootless container
  …), perms are restored and the test returns early with a clear
  `eprintln!` naming the dir and the suspected cause. On
  environments where chmod actually denies writes (this box —
  vanilla WSL ext4, plus all bare-metal Linux), the test
  exercises the EACCES path as before. See [C-4] handoff.

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
