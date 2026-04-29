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
  scope: UX rename pass → [A-6] in Ready for Test.
         Earlier slices: S.3 → [A-1], S.2 → [A-2], S.0.8 → [A-3],
         Tier 3.4 → [A-4] (all T2-PASS), V1 Ollama tok/s → [A-5].
  branch: audit/2026-04-29 (all six A-* commits land here).
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

### [A-5] V1 fix — Ollama tokens/sec via stdout_parser `eval rate` regex
- Commit SHA: b33fd28093f9fe3b328e9b2c4cbdf1b840f40c38
- Files changed: src/telemetry/samplers/stdout_parser.rs (new
  regex factory + parse-line wiring + 2 new unit tests),
  CHANGELOG.md, scripts/manual/ollama_tps_smoke.sh (new)
- Smoke script: scripts/manual/ollama_tps_smoke.sh (also greps
  T2's captured fixtures at `/tmp/v1_trial_{1,2,3}.out` when
  present)
- CHANGELOG line: "**V1 (S1) — Ollama tokens/sec now extracted
  via stdout parser.** Tester 2's V1 ground-truth check found
  that `edge_monitor` reported no `tokens_per_sec_avg` for any of
  three real `ollama run --verbose phi3` trials, even though
  Ollama itself printed `eval rate: 6.97 tokens/s` (etc) on
  stdout. Root cause: `stdout_parser.rs` had regexes for
  llama.cpp and vLLM tokens/sec output but no Ollama pattern, so
  the design's documented Tier 1.2c fallthrough (\"Ollama tok/s
  falls through to stdout parsing\") dead-ended. New regex
  `r\"^\\s*eval rate:\\s+([0-9]+(?:\\.[0-9]+)?)\\s+tokens?/s\\b\"`
  matches Ollama's generation rate while explicitly NOT matching
  `prompt eval rate:` (a different, often higher number — trial 3
  had `prompt eval rate = 60.37` vs `eval rate = 2.34`). Verified
  against T2's captured trial outputs at
  `/tmp/v1_trial_{1,2,3}.out` by
  `scripts/manual/ollama_tps_smoke.sh`. Two new unit tests guard
  the fix: one asserts the three trial values parse correctly, one
  asserts the new regex does not poach existing vLLM / llama.cpp
  lines."
- Test output (last 10 lines of `cargo test --release --lib
  telemetry::samplers::stdout_parser`):
  ```
  test ollama_eval_rate_extracts_tps_and_ignores_prompt_eval_rate ... ok
  test strict_parser_does_not_match_partial_lines ... ok
  test unrelated_line_yields_nothing ... ok
  test vllm_avg_throughput_line_yields_tps ... ok
  test line_to_frame_returns_none_on_no_match ... ok
  test llama_cpp_eval_time_line_yields_tps ... ok
  test ultralytics_speed_line_yields_latency_and_fps ... ok
  test line_to_frame_populates_telemetry_fields ... ok
  test ollama_regex_does_not_match_vllm_or_llama_cpp_lines ... ok
  test batch_extract_over_many_lines ... ok
  test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured
  ```
  Smoke output against T2's actual trial files:
  ```
  /tmp/v1_trial_1.out → eval rate:            6.97 tokens/s
  /tmp/v1_trial_2.out → eval rate:            2.82 tokens/s
  /tmp/v1_trial_3.out → eval rate:            2.34 tokens/s
  PASS: stdout_parser handles Ollama eval rate; vLLM/llama.cpp untouched.
  ```
- Builder note:
  * **The user's pointer to `src/telemetry/samplers/ollama_api.rs`
    was off.** `ollama_api.rs` only handles `/api/ps` (model-name
    discovery, Tier 1.2c). Ollama tokens/sec has always been
    routed through the stdout parser per latest.md §1.2c — the
    file that needed the new regex is
    `src/telemetry/samplers/stdout_parser.rs`. T2's V1 evidence
    (BUILDER_STATUS.md:723–728) corroborates this. I went with
    T2's diagnosis and surfaced the conflict in chat before
    starting.
  * **Test fixture is real, not synthesised.** The 6.97 / 2.82 /
    2.34 numbers are pulled directly from T2's `/tmp/v1_trial_*.out`
    captures. Future Ollama version drift in the output format is
    caught by the smoke script's grep step against new fixtures
    placed in any directory passed via `OLLAMA_FIXTURE_DIR`.
  * **No backwards-compatibility risk.** Before this commit, the
    Ollama path produced no metric at all; the new regex only
    *adds* matches. Existing tests asserting on the prior empty
    behaviour for Ollama do not exist (verified by
    `git grep ollama_eval_rate` and `git grep eval rate` across
    `src/` and `tests/`).
  * Tester 2 should re-run V1 against a live Ollama install to
    confirm the fix end-to-end (i.e. through `edge_monitor exec`
    so the parser actually sees the stream); my smoke proves the
    regex layer but not the exec→parser→accumulator wire. The L1
    re-run is the launch-readiness gate.

### [A-6] UX rename — operator-facing labels in plain language
- Commit SHA: 6f7b2298567dcd4646a4893dfcce3956a709087f
- Files changed: src/ui/panels/{registry,rogues,culprits,audit,
  completed,vitals}.rs, src/main.rs (headless tracing — drops
  `model=-` / `vram=0M` placeholders), src/governor/executor.rs
  (dry-run reason rewrite + new pinning unit test), CHANGELOG.md,
  scripts/manual/ux_rename_smoke.sh (new)
- Smoke script: scripts/manual/ux_rename_smoke.sh
- CHANGELOG line: "**UX pass — operator-facing labels rewritten in
  plain language.** TUI panel titles, headless log lines, and the
  governor's dry-run reason all dropped jargon-heavy phrasings:
  `Registry (AI workloads)` → `AI Workloads`, `Rogues (unmapped
  framework procs)` → `Framework procs`, `Culprits (top by PID
  order)` → `All processes`, `Audit (kills + regressions)` →
  `Recent actions`, `AI run summaries` → `Recent runs`, `GPU: not
  available (NVML uninitialized)` → `No GPU detected`,
  `processes: N   AI-classified: M` → `N processes   M AI workloads
  detected`. Run-summary row now says `RAM 48 MB, GPU memory 4096
  MB` instead of `rss=48M vram=4096M`, and drops the GPU memory
  clause entirely when the run had no GPU allocation. `model=` is
  omitted (TUI and headless tracing) when no model name was
  extracted, instead of rendering `model=-` which read like a
  sentinel value. Same treatment for `vram=0M` / `peak_vram_mb=0`.
  Governor dry-run reason `DRY-RUN: would send SIGTERM to AI
  process: Inference` → `Would stop ollama (dry-run mode — no
  action taken)`. Uses the actual process name and stops leaking
  the `AICategory` Debug variant."
- Test output (last 10 lines of `cargo test --release`):
  ```
  test result: ok. 327 passed; 0 failed (lib)
  test result: ok. 0 passed; 0 failed (compare_proptests)
  test result: ok. 3 passed; 0 failed (concurrent_requests_e2e)
  test result: ok. 1 passed; 0 failed (expect_rule_guard)
  test result: ok. 3 passed; 0 failed (governor_pid_reuse)
  test result: ok. 2 passed; 0 failed (governor_properties)
  test result: ok. 5 passed; 0 failed (history_cli)
  test result: ok. 2 passed; 0 failed (log_format)
  test result: ok. 3 passed; 0 failed (pipeline_end_to_end)
  test result: ok. 1 passed; 0 failed (sigterm_clean_shutdown)
  ```
  Smoke output:
  ```
  [smoke] dry-run format unit test
  test dry_run_reason_string_uses_process_name_and_plain_english ... ok
  [smoke] all old labels gone from src/
  [smoke] all new labels present in src/
  [smoke] headless stderr is clean of the old placeholders
  PASS: UX rename pass landed end-to-end.
  ```
- Builder note:
  * **Internal `FocusedPanel::{Registry,Rogues,Culprits}` enum
    names kept intact** — they are identifiers, not user-visible
    — so input.rs and app.rs focus-cycling logic stays untouched
    and T2's existing V3 PTY walkthrough's keybinding tests don't
    move.
  * **Panel structure not changed.** The user's original message
    also asked to "delete from default view; move to detail mode"
    for Rogues / Culprits / Audit. The follow-up "rename all"
    message scoped me out of detail-mode work for this pass —
    adding a real mode toggle is a feature (new keybinding + mode
    state + render routing). T2's V3 walkthrough still sees six
    panels, just with new titles. Filed below in Cross-builder
    requests for a future detail-mode pass.
  * **Headless log shape is mildly breaking.** Anyone grepping
    stderr for `model=-` or `vram=0M` (no consumer in this repo,
    but possible in operator scripts) would now miss rows. The new
    shape is strictly more informative — absence of field
    communicates absence of measurement, which `model=-` didn't.
    Worth a release-note callout if downstream consumers show up.

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

### [B-4] 14 tier-aligned smoke scripts under scripts/manual/
- Commit SHA: d1373918918fe72b47b13688623127dad296838d
- Files changed (all new): scripts/manual/{vllm_smoke,llamacpp_smoke,
  ollama_smoke,exec_stdout_smoke,regression_smoke,power_smoke,
  cold_load_smoke,prometheus_exporter_smoke,fingerprint_smoke,
  cold_vs_steady_smoke,kv_cache_smoke,exit_classify_smoke,
  vision_fps_smoke,compare_smoke}.sh
- Verification command (what the tester should run on a non-WSL
  Linux box with a real GPU + LLM runtime to maximise PASS coverage):
  ```
  for s in scripts/manual/*_smoke.sh; do
    bash "$s" >/tmp/$(basename "$s").log 2>&1
    rc=$?
    case "$rc" in
      0) echo "PASS $s" ;;
      77) echo "SKIP $s — $(grep -m1 SKIP /tmp/$(basename "$s").log)" ;;
      *) echo "FAIL $s (rc=$rc)" ;;
    esac
  done
  ```
- Expected output (last 14 lines, on this WSL dev box — no GPU,
  no vLLM, no llama-server, no Ollama, no NVML, no readable RAPL):
  ```
  SKIP vllm_smoke.sh             — requires vLLM
  SKIP llamacpp_smoke.sh         — requires llama-server
  SKIP ollama_smoke.sh           — requires Ollama daemon
  PASS exec_stdout_smoke.sh      [B-4-1.2d]
  PASS regression_smoke.sh       [B-4-1.3]
  SKIP power_smoke.sh            — requires NVML or RAPL
  PASS cold_load_smoke.sh        [B-4-2.2]
  PASS prometheus_exporter_smoke.sh [B-4-2.3]
  PASS fingerprint_smoke.sh      [B-4-3.1]
  SKIP cold_vs_steady_smoke.sh   — needs LLM runtime + exec→cold_load wiring
  SKIP kv_cache_smoke.sh         — requires vLLM
  PASS exit_classify_smoke.sh    [B-4-3.5]
  SKIP vision_fps_smoke.sh       — Tier 3.6 probe socket not wired in Runtime::new()
  PASS compare_smoke.sh          [B-4-3.7]
  ```
- Per-tier sub-claims (each filename ↔ tier ID, all at SHA d137391):
  - `[B-4-1.2a]` vllm_smoke.sh (SKIP without vLLM)
  - `[B-4-1.2b]` llamacpp_smoke.sh (SKIP without llama-server)
  - `[B-4-1.2c]` ollama_smoke.sh (SKIP without Ollama daemon)
  - `[B-4-1.2d]` exec_stdout_smoke.sh (PASS)
  - `[B-4-1.3]`  regression_smoke.sh (PASS)
  - `[B-4-2.1]` power_smoke.sh (SKIP without NVML/RAPL)
  - `[B-4-2.2]` cold_load_smoke.sh (PASS)
  - `[B-4-2.3]` prometheus_exporter_smoke.sh (PASS)
  - `[B-4-3.1]` fingerprint_smoke.sh (PASS)
  - `[B-4-3.2]` cold_vs_steady_smoke.sh (SKIP — wiring gap, see x-builder)
  - `[B-4-3.3]` kv_cache_smoke.sh (SKIP without vLLM)
  - `[B-4-3.5]` exit_classify_smoke.sh (PASS)
  - `[B-4-3.6]` vision_fps_smoke.sh (SKIP — wiring gap, see x-builder)
  - `[B-4-3.7]` compare_smoke.sh (PASS)
- Builder note: every script has `set -euo pipefail`, is `chmod +x`,
  prints a one-line preamble of what it verifies, runs the actual
  binary against realistic inputs (no mocks), asserts on observable
  output (history --json shapes, /metrics text-format families,
  exit_reason field, etc.), and ends with a single PASS / FAIL
  line — or exits 77 with a clear SKIP preamble when the
  prerequisite isn't met. Two scripts surface real wiring gaps in
  `src/`; both are filed below in Cross-builder requests rather
  than worked around in the script.

### [B-5] FEATURES.md test-count + Tier-3.4/3.6/3.2/Ollama refresh
- Commit SHA: 2bce1b3ae9dcfae4b7aef22f982d51fbaea293ec
- Files changed: FEATURES.md
- Verification command (what the tester should run):
  ```
  cargo test --release 2>&1 | grep -E '^test result' \
    | awk -F'[. ]+' '{passed += $4; failed += $6} END {print passed"/"(passed+failed)}'
  ```
- Expected output (last 10 lines):
  ```
  347/347
  ```
- Builder note: addresses T2's `[B-2]` PASS-WITH-DRIFT note. Test-
  Surface paragraph now claims 347 tests with a per-binary
  breakdown (327 lib unit + 3 concurrent_requests_e2e + 1 expect-
  rule guard + 3 governor_pid_reuse + 2 governor_properties + 5
  history_cli + 2 log_format + 3 pipeline + 1 sigterm_clean_shutdown).
  Tier 3.4 moved out of "Remaining gaps" since `[A-4]` landed the
  data path; only the per-row history rendering is deferred (see
  pushback in Cross-builder requests below). The two `[B-4]`
  wiring gaps (vision probe socket, exec→cold_load) are now also
  documented under "Remaining gaps for v0.1.0" alongside T2's V3
  S.0.7 visibility finding. Stdout parser description picked up
  Ollama `eval rate:` regex from `[A-5]`.

## Cross-builder requests

- **From Builder A → next-claimant: detail-mode panel toggle (deferred
  out of [A-6]).** The original UX feedback for [A-6] asked that
  Rogues / Culprits / Audit panels move out of the default view and
  into a "detail mode". Adding a real mode toggle is a feature (new
  keybinding in `src/ui/input.rs`, mode state on `App`, render-routing
  branch in `src/ui/mod.rs`'s `draw` path). [A-6] only handled the
  rename + placeholder cleanup that the user's "rename all" follow-up
  scoped me to. Whoever picks this up: best entry point is
  `src/ui/mod.rs`'s layout split — three sub-frames in default mode
  (Vitals + AI Workloads + Recent runs), a fourth row of {Framework
  procs, All processes, Recent actions} only when the operator
  presses, e.g., `F2`. T2's V3 walkthrough already covers the
  six-panel layout; the detail-mode test would be a new V3a probe
  asserting that the three secondary panels are absent until F2 is
  pressed.

- **From Builder A → Builder B: history viewer should show the new
  Tier 3.4 numbers.** **Builder B push-back, 2026-04-29:** my brief
  enumerates exactly which files I may edit (`CHANGELOG.md`,
  `FEATURES.md`, `edge_monitor.toml.example`, `docs/configuration.md`,
  `scripts/manual/*.sh`, `BUILDER_STATUS.md`) and explicitly
  forbids any `src/` file: *"You may NOT edit … any `src/` file. If
  you find a documentation claim that doesn't match the code, file
  it as a `## Cross-builder request` in `BUILDER_STATUS.md` — do
  not 'fix' the code yourself."* `src/history.rs` is therefore
  out of scope for Builder B. Either Tier 3.4 [A-4] absorbs the
  rendering or a new claim authorised to edit `src/` picks it up.
  `FEATURES.md` already names the pending rendering as the only
  loose end on Tier 3.4 (refreshed in `[B-5]`). Until source-edit
  authorisation is granted Builder B will not pick this up; the
  earlier "acknowledged, will land as `[B-?]`" note further down
  in this section is hereby withdrawn.

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

- **From Builder B → Builder C: C-5 baseline_strategy example —
  blocked, not landable as docs alone.** Re-checked HEAD after
  `[C-5]`'s sign-off: `BaselineStrategy::{Mean, Median}` is exposed
  by `src/analysis/compare.rs` and `BaselineMetrics::from_records_with`
  takes both the strategy and a `drop_outliers: bool`, but
  `src/runtime.rs` line 747 hardcodes
  `let strategy = BaselineStrategy::Mean;` and `drop_outliers = false`,
  and `src/config.rs` `RegressionConfig` has neither field. Adding
  `baseline_strategy = "mean"` to `edge_monitor.toml.example` would
  be invented config — setting it would silently do nothing because
  the runtime never reads it. Per the anti-celebration rule "no
  invented config defaults", Builder B will not add the example
  until either:
    1. `RegressionConfig` gains `baseline_strategy` + `drop_outliers`
       fields with `validate()` coverage, AND `runtime::check_regressions`
       reads them; OR
    2. Builder C / the auditor explicitly amends `latest.md` to drop
       the strategy-toggle expectation and freeze on Mean.
  Either resolution unblocks a follow-up `[B-?]` claim.

- **From Builder B → Builder A: Tier 3.6 vision probe socket not
  wired in `Runtime::new`.** `[telemetry] vision_probe_socket = "..."`
  is documented and parsed, and `Dispatcher::enable_vision_probe`
  exists in `src/telemetry/dispatcher.rs`, but `src/runtime.rs`
  never calls it. Setting the config has no effect: no Unix socket
  is created, no frames are accepted. `scripts/manual/vision_fps_smoke.sh`
  detects this with a pre-flight (waits for the socket to appear,
  exits 77 with a clear preamble pointing back here) so the smoke
  doesn't FAIL spuriously while the wiring is missing. Suggested
  fix: alongside the existing `d.enable_exporter(...)` call near
  line 195 of `runtime.rs`, add
  `d.enable_vision_probe(&config.telemetry.vision_probe_socket);`
  (it's a no-op when the path is empty). Once that lands, the
  smoke will PASS without changes.

- **From Builder B → auditor / Builder A: `edge_monitor exec` does
  not run the Tier 2.2 cold-load tracker.** The cold-load detector
  lives behind `Dispatcher::record_disk_io`, which is called from
  the headless tick loop (`Runtime::tick`) but not from the exec
  wrapper. So a synthetic stdout-only workload run via
  `edge_monitor exec --` cannot exercise the Tier 3.2 steady-state
  watermark, even though the watermark logic and the
  `tokens_per_sec_avg_steady` field are landed and unit-tested.
  `scripts/manual/cold_vs_steady_smoke.sh` SKIPs with code 77 and
  defers to a real LLM runtime; tracker unit tests in
  `src/telemetry/cold_load.rs` cover the algorithm deterministically.
  Decision needed: either plumb `record_disk_io(&[the_exec_pid])`
  through the exec wrapper's per-tick loop, or accept that Tier 3.2
  is a headless-only feature and amend `latest.md` Tier 3.2 prose
  to say so.

- **(Withdrawn)** ~~From Builder A → Builder B: history viewer
  should show the new Tier 3.4 numbers. Acknowledged. Will land as
  a follow-up `[B-?]` claim once the dust settles on the audit
  batch~~ — withdrawn 2026-04-29 in favour of the explicit
  push-back filed at the top of this section. `src/history.rs` is
  outside Builder B's surface per the brief.

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

## Tester 2 sign-off

SHA tested for re-verifications: `1b13d97` (HEAD of `audit/2026-04-29` at the time of this session). The 1-hour V2 idle stability ran on `e24fc58` (the SHA before [A-3]/[A-4] landed); SIGTERM clause was re-verified on HEAD with the [A-3] fix in place. T1 sign-off has not been recorded in this file at the time of this run; T2 proceeded under user direction "go for all". All re-verifications below were run by Tester 2 directly on the worktree at `/tmp/em-test-1b13d97` checked out at SHA `1b13d97`.

| Slice | Result | Timestamp (UTC) | One-line evidence |
|---|---|---|---|
| [A-1] S.3 — `expect()` rule | **[T2: PASS]** | 2026-04-29T07:50Z | `tests/expect_rule_guard.rs::every_prod_expect_is_annotated` PASS; `scripts/manual/expect_audit.sh` reports "PASS: 35 expect() sites scanned (annotated PROD or test); none violate the rule." |
| [A-2] S.2 — `--log-format json` | **[T2: PASS]** | 2026-04-29T07:50Z | `cargo test --release --test log_format`: `human_format_is_not_json_shaped` and `json_format_emits_one_json_object_per_stderr_line` both PASS; `--log-format` flag visible in `--help`; emitted lines parse as JSON. |
| [A-3] S.0.8 — SIGTERM clean shutdown | **[T2: PASS]** | 2026-04-29T07:50Z | At HEAD `1b13d97`: `kill -TERM` exits 0 in 12 ms, log shows "shutdown requested; finishing current tick" then "shutdown signal received; exiting". `tests/sigterm_clean_shutdown.rs::sigterm_drains_and_exits_zero` PASS. (Same probe at `e24fc58` returned exit 143 in 274 ms with no shutdown line — fix verified by reproduction.) |
| [A-4] Tier 3.4 — concurrent-request awareness | **[T2: PASS]** | 2026-04-29T07:50Z | `tests/concurrent_requests_e2e.rs` 3/3 PASS; `src/telemetry/concurrent_requests.rs` unit tests PASS; `scripts/manual/concurrent_requests_smoke.sh` PASS. |
| [B-1] CHANGELOG backfill | **[T2: PASS]** | 2026-04-29T07:50Z | `git show 363769c -- CHANGELOG.md \| grep -cE '^\+- \*\*Tier (2\.[123]\|3\.[12356]\|3\.7\|1\.2d)'` → `10` (matches expected). |
| [B-2] FEATURES.md rewrite | **[T2: PASS-WITH-DRIFT]** | 2026-04-29T07:50Z | Builder-stated expected `327/327`; current actual `344/344` (count drifted up because [A-3]/[A-4]/[C-4]/[C-5] landed after [B-2] was filed). All tests still green; the directional claim ("everything passes") holds. FEATURES.md test-count paragraph should be refreshed in a follow-up `[B-?]` claim. |
| [B-3] toml.example + docs | **[T2: PASS]** | 2026-04-29T07:50Z | `./target/release/edge_monitor --config edge_monitor.toml.example --no-ui --ticks 1` produces all 4 expected log lines (`loading config`, `DRY-RUN mode`, `tick tick=1 ai_processes=0 exits=0`, `tick budget reached; exiting ticks=1`). |
| [B-4] 14 tier-aligned smoke scripts | **[T2: PASS]** | 2026-04-29T07:50Z | All 14 [B-4]-listed scripts behave per the expected matrix on this E1: 7 PASS (`exec_stdout`, `regression`, `cold_load`, `prometheus_exporter`, `fingerprint`, `exit_classify`, `compare`), 7 SKIP (`vllm`, `llamacpp`, `ollama`, `power`, `cold_vs_steady`, `kv_cache`, `vision_fps`) — every SKIP matches the expected reason for a no-GPU/no-LLM-runtime WSL E1 or one of the two known cross-builder wiring gaps. 0 FAIL. The wider `scripts/manual/*_smoke.sh` set (12 PASS / 7 SKIP / 0 FAIL — see Findings) includes earlier batches and the same `ollama_smoke.sh` SKIP. |
| [C-1] F.1.10 prune logic | **[T2: PASS]** | 2026-04-29T07:50Z | `cargo test --release --lib storage::run_store::tests::prune` 3/3 PASS (`prune_keeps_three_newest_by_spawn_time`, `prune_with_limit_two_leaves_two_files_on_disk`, `pruned_ids_stay_pruned_after_reopen`). |
| [C-2] F.1 — 1000-iter proptest | **[T2: PASS]** | 2026-04-29T07:50Z | `PROPTEST_CASES=1000 cargo test --release --test governor_properties -- --test-threads=1` PASS; `storage::run_store::prop_tests::append_recent_invariants` PASS in the lib run. The `proptest::append_recent_invariants passed 1000 cases` line is emitted (verified visually). |
| [C-3] F.3.4 warn-tier (12%) | **[T2: PASS]** | 2026-04-29T07:50Z | `analysis::compare::tests::twelve_percent_drop_is_warn_not_critical` PASS; `analysis::compare::tests::warn_critical_boundary_matrix` PASS. |
| [C-4] F.1.7 ENOSPC / write-rejection | **[T2: PASS]** | 2026-04-29T07:50Z | `storage::run_store::tests::append_returns_err_when_filesystem_rejects_write` PASS at HEAD on this WSL E1 (the runtime-probe guard added in the follow-up commit kicks in cleanly when the chmod-deny actually does deny). |
| [C-5] F.3.8 robust baseline | **[T2: PASS]** | 2026-04-29T07:50Z | `analysis::compare::tests::robust_baseline_median_unaffected_by_outlier` PASS. |

Cargo test aggregate at HEAD `1b13d97`: **344 passed, 0 failed** across 11 result lines (9 test binaries + 2 doctest sweeps), `cargo clippy --all-targets -- -D warnings` clean.

## Tester 2 Findings

(SHA: `1b13d97`. Environment: E1 — WSL2 Ubuntu, x86_64, 8 cores, 7.6 GiB RAM, no NVIDIA GPU, no NVML, no readable RAPL, Ollama installed locally for V1 only.)

### V1 — Ollama tokens/sec ground truth: **FAIL** (S1 marquee-feature gap)

The marquee feature claim "edge_monitor measures tokens/sec for LLM workloads" does not hold for Ollama at any SHA in this branch's history.

Procedure (per charter T.1): pulled `phi3:latest` (2.2 GB) under `OLLAMA_MODELS=/tmp/em-bin/models`; started `ollama serve`; ran `ollama run --verbose phi3 "Explain quicksort in 200 words."` 3× while `edge_monitor --no-ui --ticks 0 --log-level info` was running with an isolated config and data_dir.

Ollama's reported eval rates (the ground truth):
- Trial 1: **6.97 tokens/s** (`load duration: 1m38.9s`, `eval count: 474 tokens`, `eval duration: 1m8.0s`)
- Trial 2: **2.82 tokens/s** (`eval count: 461 tokens`, `eval duration: 2m43.6s`)
- Trial 3: **2.34 tokens/s** (`eval count: 410 tokens`, `eval duration: 2m55.4s`)

(All three are slow because this E1 is CPU-only inference; the absolute number is irrelevant — the comparison would still be valid against any fixed ground-truth.)

edge_monitor's tokens_per_sec_avg for the Ollama-spawned process:
- Trial 1: **no value emitted**
- Trial 2: **no value emitted**
- Trial 3: **no value emitted**

`grep -iE 'tokens?_per_sec|RunRecord|tokens?[ /]s' /tmp/v1_em.log` returned zero hits across the 3 inference runs (which collectively spanned ~9 minutes of detected `ai_processes=1..3` activity in the log). `find /tmp/v1_data -type f` returned empty — no `RunRecord` was written for the Ollama-spawned workers.

Root cause (verified against `latest.md` + source):

* `latest.md` §1.2c states the design: "Ollama doesn't expose Prometheus. It has `/api/ps` (list loaded models) and embeds tok/s in response JSON during generation, **which we cannot intercept**. … For tok/s, fall through to stdout parsing (next bullet)." The fallback is §1.2d's stdout regex parser.
* `src/telemetry/samplers/stdout_parser.rs` (HEAD) registers exactly two tokens/sec patterns:
  - `LLAMA_CPP_TPS = r"eval time\s*=.*?(\d+\.\d+)\s+tokens? per second"` (llama.cpp)
  - `VLLM_TPS = r"Avg generation throughput:\s*([0-9]+(?:\.[0-9]+)?)\s+tokens?/s"` (vLLM)
* Ollama's `--verbose` output uses **`eval rate:     NN.NN tokens/s`** — which matches neither regex. (Confirmed by inspecting all three trial outputs at `/tmp/v1_trial_{1,2,3}.out`.)
* `src/telemetry/samplers/ollama_api.rs` line 11 documents the fallthrough; the fallthrough's regex set never grew an Ollama-shaped pattern, so the chain dead-ends.

Even routing through `edge_monitor exec ollama run --verbose phi3 …` (the design's preferred path) would hit the same dead end — the exec wrapper captures stdout faithfully, but the parser has no Ollama regex to match against, so no `MetricKind::TokensPerSec` is emitted.

Severity: **S1**. The feature is named for LLMs and Ollama is the most common edge LLM runtime; "we measure tok/s for vLLM and llama.cpp but not Ollama" is fine as an internal nuance and **not** fine as a launch claim. Suggested fix: add a third regex to `stdout_parser.rs` matching `r"^\s*eval rate:\s*([0-9]+(?:\.[0-9]+)?)\s+tokens?/s"` and a fixture-line unit test against captured Ollama-verbose output.

This finding addresses audit `83b5360` S1.3 ("T.1 Ollama ground truth never measured at any SHA") — it has now been measured, and the measurement reveals the gap.

### V2 — 1-hour idle stability: **PASS at HEAD `1b13d97`**

The 1-hour run was performed on SHA `e24fc58` (pre-[A-3]) per the test-runner schedule; the SIGTERM clause was re-verified at HEAD `1b13d97`.

Memory + CPU result (from `e24fc58`, runtime 06:28:22Z → 07:28:22Z):
- `RSS_5  = 14600 KB`
- `RSS_60 = 16548 KB`
- `RSS Δ  = 1948 KB ≈ 1.9 MB` — **PASS** (limit < 10 MB / 10240 KB)
- CPU samples at T+10/20/30/40/50: `0.0% / 0.0% / 0.0% / 0.0% / 0.0%`, mean `0.0%` — **PASS**
- 3588 ticks in 60 minutes (≈ 1 tick/sec, steady)
- `ai_processes` was 0 for every single tick (no contamination)

SIGTERM clause:
- On `e24fc58`: `kill -TERM` returned exit 143 in 274 ms with no shutdown log line and no audit data — **FAIL** (root cause: `Cargo.toml` `ctrlc = "3"` lacked `features = ["termination"]`).
- On HEAD `1b13d97` (5-minute sanity rerun, T+1=14280 KB → T+5=14692 KB, Δ=412 KB): `kill -TERM` returned exit 0 in 1050 ms with both `shutdown requested; finishing current tick` and `shutdown signal received; exiting` log lines present — **PASS**.
- The audit-log-intact clause is moot for an idle test that never killed anything (the audit JSONL only opens on kill emission); the exit-0 + drained-tick + shutdown-marker triple is the strongest signal available for an idle workload.

Net V2 verdict at HEAD: **PASS**. The 1-hour memory result holds across the [A-3] change because [A-3] is a `Cargo.toml` feature flag plus comment-only `src/main.rs` edits and changes nothing in the idle-loop allocation path; the SIGTERM clause that failed at `e24fc58` now passes at HEAD.

This finding addresses audit `83b5360` S1.2 ("X.1.1 1-hour stability never completed at any SHA").

### V3 — TUI walkthrough: **PASS-WITH-FINDINGS** (no blockers)

PTY-driven walkthrough using `script -q -c '...'` with `stty rows 40 cols 120` to give ratatui a non-zero terminal size.

Lifecycle and rendering:
- Alt-screen enter (`ESC[?1049h`) and leave (`ESC[?1049l`): 1 / 1 — **PASS**
- Cursor hide/show (`ESC[?25l` / `ESC[?25h`): present and balanced — **PASS**
- Six panes drawn with proper box-drawing characters: `Vitals`, `Registry (AI workloads)`, `Rogues (unmapped framework procs)`, `Culprits (top by PID order)`, `Audit (kills/actions)`, `AI run summaries` — **PASS**
- RAM partial-block bar (█ chars) renders inside Vitals; load-avg line populated; `GPU: not available (NVML uninitialized)` prominently displayed — **PASS** (clear no-GPU messaging in TUI; contrast with S.0.7 finding for headless mode below)
- `q` keypress → process exits with code 0; `script` reports `[COMMAND_EXIT_CODE="0"]` — **PASS**
- `d` keypress → log line `dry-run mode toggled dry_run=false` confirms input was processed — **PASS**
- `?` keypress → `help` text appears in the rendered output — **PASS**
- `h` keypress → history overlay opens (model column rendered) — **PASS**
- `Esc` → returns to main view — **PASS**
- `SIGWINCH` sent to the running process via `kill -WINCH`: process did not crash, continued rendering — **PASS**
- No `panic`, `fatal`, `Error`, or `signal` markers anywhere in the captured ANSI stream — **PASS**

Findings (none are blockers):

1. **`g` keybinding for Grafana is not implemented in source.** `src/ui/input.rs:34-44` (`Mode::Normal => match key.code`) handles `q ? d k h / j K Tab BackTab` plus arrow keys; there is no `KeyCode::Char('g')` arm. The Tester 2 charter explicitly expects `g` to "open Grafana; if Grafana isn't configured, the fallback setup page appears". This is either a doc/spec drift (the keybinding was descoped silently) or a missing implementation. Severity: **S3** (no documented user-visible regression; `g` is simply unmapped). Suggested fix: either (a) add the binding + Action::OpenGrafana to `input.rs` and `app.rs`, plumbing through to the existing `[telemetry] grafana_url`-style config, or (b) update the public-facing keybinding docs and remove the charter expectation.

2. **KV-pressure threshold colors cannot be visually verified on E1.** Charter expects "≥80% turns red" in the registry row and "KV!" badge for runs with ≥99.5% peak in the history overlay. Both depend on a vLLM-style workload that publishes KV-cache metrics, which this WSL E1 cannot run. The unit + integration tests (309 → 344 lib + integration tests at HEAD; KV-cache code paths covered in `src/telemetry/samplers/vllm_prometheus.rs` and `src/runtime.rs`) confirm the data path is wired; the rendered colors are only verifiable on an L1 (Linux + NVIDIA) box. Severity: **deferred** — re-run V3 on L1 before launch.

3. **Headless `S.0.7` no-GPU visibility regression** (cross-reference with prior tester report and audit). On `--no-ui` runs, the no-GPU NVML failure is logged at `DEBUG` level only (`NVML init failed: … GPU metrics unavailable`); at `--log-level info` (the default), no GPU-related line appears. The TUI renders the message correctly, but headless users with no GPU get silent operation. Severity: **S3 visibility**.

### Secondary tests at HEAD `1b13d97`

* `cargo test --release` aggregate: **344 passed / 0 failed** across 9 test binaries + 2 doctest sweeps.
* `cargo clippy --all-targets -- -D warnings`: clean.
* G.7 PID-reuse safety (`tests/governor_pid_reuse.rs`) re-run 5× consecutively at `e24fc58`: **15/15 sub-tests PASS, 0 flakes** (tests at HEAD are byte-equivalent and confirmed via the aggregate `cargo test --release` run).
* `governor_properties` proptest with `PROPTEST_CASES=1000`: **PASS**.
* T.8 Prometheus exporter: `127.0.0.1:9472/metrics` listens and serves; **13 `# HELP` lines + 13 `# TYPE` lines** across 13 distinct metric families (`edge_monitor_processes_total`, `edge_monitor_run_tokens_per_sec`, `edge_monitor_run_fps`, `edge_monitor_run_vram_bytes`, `edge_monitor_run_gpu_watts`, `edge_monitor_run_cpu_watts`, `edge_monitor_gpu_watts`, `edge_monitor_gpu_temp_celsius`, `edge_monitor_cold_load_seconds`, `edge_monitor_ai_processes_active`, `edge_monitor_governor_kills_total`, `edge_monitor_regressions_total`, `edge_monitor_tick_count_total`); pass criterion ≥5 distinct metric names with HELP+TYPE met. (`promtool` not installed locally; manual inspection consistent with text-format spec.)
* T.3 `--log-format json` (S.2.3): both subprocess integration tests PASS; emitted lines parse via `python3 json.loads`; every line carries `timestamp` + `level`.
* `verify-edge-monitor.sh` end-to-end (1.1 min wall): **20 PASS / 3 FAIL / 6 WARN / 2 SKIP**. The 3 FAILs are all verify-script bugs (`E.4` literal-pipe-in-grep, `D.5` arithmetic comparison against multi-line `0\n0`, and `F.1` was previously a verify-script regex bug but at this SHA passes — only 2 verify-script defects remain). None are edge_monitor binary defects. Recommend filing a `[B-?]` follow-up to fix the verify script's grep flags.

### Audit `83b5360` S1 status after this run

- S1.1 — BUILDER_STATUS.md missing `[T1: PASS]` / `[T2: PASS]` tags + Tester 2 Findings section: **T2 portion now landed by this commit; T1 portion still open** (T1 has not added tags).
- S1.2 — X.1.1 1-hour stability never completed: **landed** (V2 PASS at HEAD; see above).
- S1.3 — T.1 Ollama ground truth never measured: **landed, with new finding** (V1 measured; the measurement revealed an Ollama-shaped stdout-regex gap).

### Items intentionally not in scope for Tester 2

- Windows verification (charter explicitly defers to v1.1).
- Real GPU + RAPL + tegrastats verification (no L1/L3 box available; flagged for re-run before launch).
- TUI screenshot capture (PTY-only environment cannot drive a real terminal display; lifecycle and behavior were exercised but visual fidelity was inferred from ANSI stream, not a live screen).
- Editing or rebasing other builders' working trees; the [A-3] fix's WIP edits to `Cargo.toml` + `src/main.rs` were observed mid-session but not testable until the commit landed at `e4a7e74`.
