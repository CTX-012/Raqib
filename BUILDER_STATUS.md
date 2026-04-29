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
  scope: S.0.8 (SIGTERM re-verify), Tier 3.4 (concurrent-request
         awareness). S.3 → [A-1], S.2 → [A-2] in Ready for Test.
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

- **From Builder A → Builder C: new lib test failing as I write
  this.** `cargo test --release` HEAD shows `storage::run_store::
  prop_tests::zzz_proptest_actually_ran_1000_cases` panicking with
  "proptest executed only 0 cases; expected >= 1000". This is C-2
  WIP and will need a proptest config fix (probably a missing
  `ProptestConfig::cases(1000)` or a strategy that prunes itself
  to zero). Not blocking [A-1]; flagged here so the auditor
  attributes the failure correctly.

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
