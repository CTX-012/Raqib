# edge_monitor — Critical Test Pass against TEST.md

- **Tester:** Claude (critical-tester role, on the user's solo dev box)
- **Date:** 2026-04-28
- **Commit (HEAD):** `1af85b0` (`feat(telemetry): Ollama /api/ps sampler (latest.md Tier 1.2c) + frame hardening`) with one **uncommitted** local change in `src/config.rs` (adds `TelemetryConfig`, required by HEAD's `runtime.rs` to compile — see Findings).
- **Environment (E1 only):** Ubuntu/WSL2, Linux 5.15.167.4-microsoft-standard-WSL2, no NVIDIA GPU, no NVML, no real hardware power/thermal counters.
- **Out-of-scope by environment:** E2 (no-GPU is partly tested here; this *is* E2), E3 (Windows), E4 (WSL+NVIDIA passthrough — we have WSL but no GPU), E5 (no Jetson). All non-E1 tests are honestly SKIPped.

This report follows TEST.md's own format: every test ID with PASS/FAIL/SKIP, severity on FAIL, evidence, and reproduction. Entries are grouped by phase.

---

## Headline findings

1. **S1 — SIGTERM not handled cleanly (S.0.8 FAIL).** The headless binary exits with code 143 (kernel-default SIGTERM) instead of running the shutdown handler. Root cause: `Cargo.toml` uses `ctrlc = "3"` without the `termination` feature, so only SIGINT is caught. Audit log is not flushed on SIGTERM. Fix: enable the `termination` feature, or add a manual SIGTERM handler. CLAUDE.md safety rule 6 (durable audit trail) is at risk on SIGTERM-driven shutdowns.

2. **S0 — PID-reuse safety (G.7) NOT implemented or tested.** Reading `src/governor/executor.rs`, kills do not record/verify the target process's start time before SIGKILL. Between SIGTERM and SIGKILL, the kernel can reassign the PID — TEST.md flags this as S0. There is no test exercising PID reuse, and no observable code path that compares `/proc/<pid>/stat`'s starttime field across the grace window. **Treat this as a launch blocker.**

3. **Phase-2 features partly absent.** TEST.md tests for the **Prometheus exporter** (PE.\*), **exec wrapper** (T.4), **fingerprinting** (FP.\*), **cold-start I/O** (C.\*), **power & thermals** (P.\*), **vision FPS** (V.\*), and **`compare` CLI** (Tier 3.7) all reference features that are not present in HEAD. They are SKIPped, but the report flags each so the launch checklist is honest. Per `latest.md` these are Tier 2.x / 3.x — acceptable to defer, but they must not be claimed as "tested".

4. **Phase-1 coverage gaps for the *implemented* foundations.** F.1.6 (concurrent append), F.1.7 (disk full), F.1.10 (`keep_runs_per_model` pruning), F.1.11 (10k-record perf <100ms), and the F.1 property test are missing. F.1.8 / F.1.9 (path traversal / Unicode) are **moot by design** — model name never appears in a filesystem path; runs are stored as `runs/<date>/run-<uuid>.json`.

5. **Phase-1 regression detector** has no Warn-tier (12%) test (F.3.4) or outlier/robust-baseline test (F.3.8). Critical-tier (F.3.5), no-regression (F.3.3, F.3.6), and inverted-direction (F.3.7) are well covered. Empty/tiny baseline (F.3.1/F.3.2) are guarded by `min_baseline_samples=3` and one explicit test.

6. **Transient build inconsistency observed once.** A first `cargo test --release --all` failed with E0425 `cannot find function build_dispatcher`. A second invocation succeeded with 252 unit + 10 integration tests passing. Treated as a stale-incremental cache artifact (not reproducible after `cargo build --release`), but worth flagging.

---

## Phase 0 — Smoke

| ID | Test | Result | Severity | Evidence | Repro |
|---|---|---|---|---|---|
| **S.0.1** | `--version` exits 0, prints semver | **PASS** | — | `edge_monitor 0.1.0` matches `Cargo.toml` `version = "0.1.0"`, exit 0 | `./target/release/edge_monitor --version` |
| **S.0.2** | `--help` lists every flag | **PASS** | — | All flags in `Cli` struct (`--config`, `--dry-run`, `--no-ui`, `--ticks`, `--log-level`) appear; `history` subcommand listed | `./target/release/edge_monitor --help` |
| **S.0.3** | `--no-ui --ticks 5` exits ≤6 s, prints ≥5 ticks | **PASS** | — | `real 0m4.033s`, 5 tick lines emitted, exit 0 | `time ./target/release/edge_monitor --no-ui --ticks 5` |
| **S.0.4** | TUI starts and `q` quits cleanly | **SKIP** | — | Not exercised non-interactively in this pass; the `q` quit path is covered by `ui::input::tests::q_quits_in_normal_mode` and the `ui::app::tests::quit_request_propagates` unit tests. Manual TUI run not performed in this WSL session. | Interactive: launch and press `q` |
| **S.0.5** | Bad config rejected with line-numbered error | **PASS** | — | `printf 'garbage{[\n' > /tmp/bad.toml`; binary exits 1 with `TOML parse error at line 1, column 8` and a caret pointer | `./target/release/edge_monitor --config /tmp/bad.toml` |
| **S.0.6** | `tick_interval_ms = 0` rejected before runtime | **PASS** | — | `[runtime]\ntick_interval_ms = 0` → `Error: invalid config: runtime.tick_interval_ms must be > 0` (exit 1). Validation lives in `src/config.rs:213` (`Config::validate`). Note: top-level `tick_interval_ms = 0` (no section) is silently ignored by serde — that's correct TOML semantics, not a config bug. | See S.0.6 fix in source |
| **S.0.7** | No-GPU host doesn't crash, shows "GPU unavailable" | **PASS (S3 caveat)** | S3 if user-visibility wanted | Default `--log-level info`: no GPU-related message appears. At `--log-level debug`: `NVML init failed: a libloading error occurred: libnvidia-ml.so.1: cannot open shared object file ... GPU metrics unavailable`. Binary runs all 5 ticks without crash. The TEST.md-prescribed "GPU unavailable" message is only visible at debug level — consider promoting to one-shot info. | `./target/release/edge_monitor --no-ui --ticks 2 --log-level debug` |
| **S.0.8** | SIGTERM exits cleanly within 2 s, audit flushed | **FAIL** | **S1** | Started `--no-ui --ticks 60`, sent `SIGTERM` after 5 s, `wait` returned exit code **143** (= 128 + SIGTERM). The shutdown handler did NOT run; only SIGINT does (verified separately: SIGINT yielded exit 0 with `shutdown signal received; exiting`). Root cause: `Cargo.toml` line `ctrlc = "3"` lacks the `termination` feature. | `./em --no-ui --ticks 60 & PID=$!; sleep 5; kill -TERM $PID; wait $PID; echo $?` |
| **S.0.9** | Binary size <10 MB Linux | **PASS** | — | `ls -lh target/release/edge_monitor` → 7.4 MB (post-clippy build). Earlier strip-enabled build measured 3.5 MB. Both well under 10 MB. | `ls -lh target/release/edge_monitor` |

**Phase 0 verdict:** 7/9 PASS, 1 FAIL (S.0.8 SIGTERM), 1 SKIP (S.0.4 interactive TUI). The FAIL is S1 and blocks the launch checklist's "Phase 0 passes 100%" gate.

---

## Phase 1 — Foundation

Run via `cargo test --release --all`. Total: **252 unit + 10 integration tests, 0 failed**, ~0.4 s.

### F.1 — RunStore

| ID | Test | Result | Mapped to | Notes |
|---|---|---|---|---|
| F.1.1 | Append 1, retrieve 1 round-trips | **PASS** | `storage::run_store::tests::appends_then_reopens_with_full_index` | |
| F.1.2 | Append 10, recent newest-first | **PASS** | `storage::run_store::tests::recent_returns_newest_first` | |
| F.1.3 | Cross-model isolation | **PASS** | `list_models_returns_all_keys_sorted` + `recent_returns_newest_first` (which also asserts model A doesn't bleed into B's recent) | |
| F.1.4 | Restart simulation, rebuild index | **PASS** | `appends_then_reopens_with_full_index`, `record_file_exists_for_every_index_entry` | |
| F.1.5 | Corrupted index line warn-logged, others readable | **PASS** | `corrupted_index_line_is_skipped_not_fatal` | |
| F.1.6 | Concurrent append: 2 threads × 500 → 1000, no panic | **SKIP (by design)** | — | `RunStore::append` takes `&mut self`; the type is single-writer (documented as such in source). True concurrent append cannot compile, so the spec is satisfied at the type-system level. |
| F.1.7 | Disk full → error returned cleanly | **NOT TESTED** | — | No test covers `WriteRecord` / `OpenIndex` ENOSPC path. Code returns `RunStoreError::WriteRecord { source: io::Error }` so a panic is unlikely, but unverified. Recommend adding one tmpfs-backed test. |
| F.1.8 | Path traversal in model name (`"../../etc/passwd"`) sanitized | **PASS (moot by design)** | — | Reading `run_store.rs:319 append`, the on-disk path is `runs/<YYYY-MM-DD>/run-<UUID>.json`. Model name is only used as an in-memory `HashMap` key and inside the JSON content — never touches the filesystem path. The TEST.md concern does not apply. |
| F.1.9 | Unicode model name round-trips | **PASS (moot by design)** | — | Same reason as F.1.8 — Unicode in the model name only affects HashMap lookups and JSON encoding (handled by serde). |
| F.1.10 | `keep_runs_per_model` cap enforced (prune oldest) | **NOT TESTED + NOT IMPLEMENTED** | — | `StorageConfig::keep_runs_per_model` is parsed from TOML and validated > 0, but `RunStore::append` does not invoke any prune logic. Comment in `latest.md` Tier 1.1 calls this out as "reserved field, pruning later". Per TEST.md as written, this is a SKIP for unimplemented feature, but make it explicit in the launch notes. |
| F.1.11 | 10 000 records, `recent(model, 20)` <100 ms | **NOT TESTED** | — | No perf benchmark exists. Code shape (`Vec<RunId>` per model, reverse-iterate) is O(N+1) for the rev() and O(20) reads, but unmeasured. |
| **F.1 property test** | `append_recent_invariant` (1000 cases) | **NOT PRESENT** | — | No `proptest!` block in `src/storage/run_store.rs`. The only project-wide proptests are in `tests/governor_properties.rs`. |

### F.2 — Telemetry sampler isolation

| ID | Test | Result | Mapped to |
|---|---|---|---|
| F.2.1 | Panicking sampler doesn't crash runtime | **PASS** | `telemetry::dispatcher::tests::dispatcher_survives_panicking_sampler` |
| F.2.2 | Slow sampler doesn't block tick loop | **PASS** | `telemetry::dispatcher::tests::dispatcher_timeout_protects_against_slow_samplers` |
| F.2.3 | NaN/Infinity coerced to None, warn-logged | **PASS** | `telemetry::accumulator::tests::nan_tps_is_skipped_not_recorded` |
| F.2.4 | Negative tok/s rejected | **PASS** | `telemetry::accumulator::tests::negative_tps_is_rejected` |
| F.2.5 | 1e18 tok/s clamped/rejected | **PASS** | `telemetry::accumulator::tests::impossibly_large_tps_is_rejected` |

### F.3 — Regression math

| ID | Test | Result | Mapped to / Notes |
|---|---|---|---|
| F.3.1 | Empty baseline → no regressions | **PASS (by guard)** | `detect_regressions` guards on `baseline.sample_size < cfg.min_baseline_samples`. No explicit empty-baseline test, but subsumed by F.3.2. |
| F.3.2 | n=2 baseline → no regressions | **PASS** | `tiny_baseline_emits_no_regressions` |
| F.3.3 | Stable baseline + matching record → no regressions | **PASS** | `matching_record_no_regressions` |
| F.3.4 | 12% regression → Warn fires | **NOT TESTED** | The codepath at `compare.rs:238` returns `Severity::Warn` when `delta_pct ≥ warn_pct && < critical_pct`, but no unit test directly hits that band. Recommend a 12-tps drop test against a 40-tps baseline. |
| F.3.5 | 27% regression → Critical | **PASS** | `slow_run_is_critical` (uses 28-vs-40 = 30% drop) |
| F.3.6 | Improvement → no false positive | **PASS** | `faster_run_is_not_a_regression` |
| F.3.7 | Inverted-direction (peak_rss rising IS a regression) | **PASS** | `higher_rss_is_a_regression` |
| F.3.8 | Baseline outlier → robust median or "noisy" flag | **NOT TESTED + NOT IMPLEMENTED** | `BaselineMetrics::from_records` (per `compare.rs`) computes mean+stddev, no median fallback or noisy-baseline flag. Outlier handling is implicit (one wild value just inflates stddev). |

**Phase 1 verdict:** all *implemented* tests PASS. Documented gaps are the missing F.1 property test, F.3.4 Warn-tier test, F.1.7 disk-full handling, F.1.10 prune logic, and F.3.8 robust baselines.

---

## Phase 2 — Per-feature verification

### Tier 1.1 — History viewer (implemented)

| ID | Test | Result | Evidence |
|---|---|---|---|
| H.1 | Happy: record appears in `history` | **PASS** | Integration test `tests/history_cli.rs::appended_record_shows_up_in_history_text` and live `./em history` against `~/.local/share/edge_monitor` shows summary table for prior runs with model column populated. |
| H.2 | Adversarial: model name with `/` (`meta-llama/Llama-3-8B`) sanitized | **PASS (moot by design)** | Same as F.1.8 — model name doesn't reach the filesystem path. |
| H.3 | `--json` matches schema | **PASS** | `tests/history_cli.rs::json_output_parses_as_record_array_for_a_model` and `json_output_parses_as_summary_array_when_no_model` deserialize to typed structs. Live `./em history --json` returns valid JSON array starting `[ {`. |

### Tier 1.2 — Tokens/sec for LLMs (samplers implemented; exec wrapper NOT)

| ID | Test | Result | Notes |
|---|---|---|---|
| T.1 | Real Ollama, ground-truth comparison within 10% | **SKIP** | No Ollama installed on this WSL box; no GPU. |
| T.2 | Mock Prometheus → frame populated | **PASS** | `telemetry::samplers::vllm_prometheus::tests::end_to_end_scrape_through_local_server` and the equivalent llama.cpp / Ollama tests all spin up a local server (axum-based, in test code) and verify frame fields match canned metrics. |
| T.3 | Mock garbage / 500 / timeout — sampler doesn't spin/panic | **PASS** | `dispatcher_timeout_protects_against_slow_samplers`, `parse_metrics_extracts_named_lines`, plus the hardening tests in `telemetry::samplers::*::compute_frame_*`. |
| T.4 | `edge_monitor exec -- ...` wrapper | **SKIP (NOT IMPLEMENTED)** | `edge_monitor exec --help` → `error: unrecognized subcommand 'exec'`. Per `latest.md` Tier 1.2d this is a planned but unbuilt path. |
| T.5 | Adversarial output (NaN, 1e18 tps) rejected | **PASS** | `accumulator::tests::nan_tps_is_skipped_not_recorded`, `negative_tps_is_rejected`, `impossibly_large_tps_is_rejected`. Stdout-parser fixtures cover real `eval rate: NaN` style lines. |
| T.6 | Prometheus exporter `/metrics` exposes `edge_monitor_tokens_per_second` | **SKIP (NOT IMPLEMENTED)** | `curl http://127.0.0.1:9472/metrics` → connection refused; no listener. Tier 2.3. |

### Tier 1.3 — Regression detection (implemented)

| ID | Test | Result | Evidence |
|---|---|---|---|
| R.1 | 10 baseline @ 40, 1 @ 28 → Critical fires | **PASS** | `analysis::compare::tests::slow_run_is_critical` (and `runtime` integration plumbing in `check_regressions`). |
| R.2 | 10 baseline @ 40, 1 @ 41 → no false positive | **PASS** | `faster_run_is_not_a_regression` |
| R.3 | TUI Audit panel surfaces regression line | **SKIP (manual TUI)** | TUI audit ring is wired in `runtime.rs::check_regressions` (`emit_event(Regression{...})`), and `ui::app::tests::*` cover the panel render, but the end-to-end "press a key, see the line" path was not exercised interactively in this pass. |

### Tier 2.1 — Power & thermals

**SKIP — NOT IMPLEMENTED in HEAD.** No `gpu_watts_*` or `cpu_watts_*` accumulators exist outside the telemetry frame schema (which is unpopulated by any sampler). RAPL reader and tegrastats sampler are absent. `latest.md` schedules these for Tier 2.1, post-launch.

P.1, P.2, P.3 → **SKIP**.

### Tier 2.2 — Cold-start I/O

**SKIP — NOT IMPLEMENTED in HEAD.** `RunRecord::cold_start: Option<ColdStartStats>` exists in the schema but is always `None`; no `/proc/<pid>/io` plateau detector module is present.

C.1, C.2, C.3 → **SKIP**.

### Tier 2.3 — Prometheus exporter

**SKIP — NOT IMPLEMENTED in HEAD.** `TelemetryConfig::prometheus_bind` is a config field but no HTTP server is started anywhere in `src/main.rs` or `src/runtime.rs`. Port 9472 is not bound.

PE.1, PE.2, PE.3 → **SKIP**.

### Tier 3.1 — Model fingerprinting

**SKIP — NOT IMPLEMENTED in HEAD.** `RunRecord::model_fingerprint: Option<String>` exists but is always populated as `None` (`run_store.rs:192`). No `fingerprint_model_file` function or sha256 hashing module.

FP.1–FP.4 → **SKIP**.

### Tier 3.5 — Exit-reason classification

**SKIP — PARTIAL**. `ExitReason` enum and an `exit_reason_classification_matrix` unit test exist in `storage::run_store`, but the dmesg/journalctl OOM probe and the spawn-fixture-driven integration tests called out by ER.1–ER.5 are not present. The matrix test covers the pure-logic classifier branches against fixture inputs only.

ER.1, ER.2 → **SKIP** (no spawn/kill fixture). ER.3 OOM → **SKIP** (cannot induce OOM safely on a dev box). ER.4 GovernorKill audit_id matching → **SKIP**. ER.5 dmesg false-attribution → **SKIP**.

### Tier 3.6 — Vision FPS

**SKIP — NOT IMPLEMENTED beyond a stdout regex**. The Ultralytics regex lives in `telemetry::samplers::stdout_parser` and is unit-tested (`ultralytics_speed_line_yields_latency_and_fps`), but there's no end-to-end YOLO ground-truth comparison in this environment.

V.1 → **SKIP**, V.2 → **SKIP**, V.3 → **PASS** (`stdout_parser` regex unit test against fixtures).

### Governor (already shipped — safety re-verification)

| ID | Test | Result | Severity (TEST.md) | Notes |
|---|---|---|---|---|
| G.1 | Allowlisted PID never killed | **PASS** | S0 | `tests/governor_properties.rs::allowlisted_processes_never_killed` (proptest, 256 default cases). |
| G.2 | Dry-run = 100 simulated runaways → zero signals | **PASS** | S0 | `governor::executor::tests::executor_evaluate_dry_run`, `governor::manual::tests::dry_run_sigterm_logs_not_kills`, `dry_run_sigkill_logs_not_kills`. |
| G.3 | Default config dry-run = true | **PASS** | S0 | `config::tests::default_is_dry_run`. |
| G.4 | SIGTERM precedes SIGKILL by ≥ grace_secs | **PASS** | S0 | `governor::tests::pending_kill_elapsed`, `governor::executor::tests::executor_evaluate_*` paths. Min grace enforced by `config.rs:225` (≥1 s). |
| G.5 | Rate limit: 4th kill in 60s blocked | **PASS** | S1 | `tests/governor_properties.rs::rate_limit_is_a_hard_ceiling`, `governor::executor::tests::executor_rate_limits_enforced_kills`. |
| G.6 | Dry-run "would-kill" doesn't consume budget | **PASS** | S1 | `governor::executor::tests::executor_rate_limit_not_consumed_in_dry_run`. |
| **G.7** | **PID reuse safety** | **FAIL — NOT IMPLEMENTED** | **S0** | Reading `src/governor/executor.rs`: `PendingKill` carries the PID + timestamp but **not the process start time** from `/proc/<pid>/stat`. When the grace expires and the executor sends SIGKILL, there is no check that the process at PID *N* is still the same one originally targeted. If the original process exits cleanly during the grace window and the kernel reuses the PID, an unrelated process gets SIGKILLed. **No test exercises this scenario.** TEST.md flags this as the single most important governor S0. |
| G.8 | EPERM on kill — logged as failure, no panic | **NOT TESTED** | S2 | The signal send path uses `nix::sys::signal::kill`, error is propagated via `KillAction::*`, but no test injects EPERM. Code review only. |
| G.9 | ESRCH (already exited) — recorded as `AlreadyExited`, not retried | **PASS** | S2 | `governor::executor::tests::executor_evaluate_exited` covers the `is_exited()` short-circuit; the ESRCH-from-syscall branch is not directly tested. |

### Classifier (already shipped — sanity)

| ID | Test | Result | Notes |
|---|---|---|---|
| CL.1 | `Microsoft.PowerShell.exe` NOT classified | **NOT TESTED EXPLICITLY** | No test name mentions PowerShell; covered indirectly by `non_ai_names_return_none`. |
| CL.2 | Chromium without AI NOT classified | **NOT TESTED EXPLICITLY** | Covered indirectly by `non_ai_names_return_none`. |
| CL.3 | vLLM in container with cmdline accessible IS classified | **PASS** | `classifier::keyword_match::tests::vllm_module_in_python_cmdline`. |
| CL.4 | Sticky model name across re-exec | **PASS** | `lifecycle::tracker::tests::lifecycle_tracker_propagates_model_name_into_summary` + sticky-model code path in `lifecycle::tracker`. |
| CL.5 | `prctl(PR_SET_NAME, "vllm")` spoof beaten by script-sniff/env | **NOT TESTED** | The script-sniff and env-var paths beat name-only matches in priority order, but no test simulates a name-spoof + non-AI cmdline. |

---

## Phase 3 — Stability (X.1)

TEST.md X.1.1 wants 1 hour idle. We ran a **best-effort short version**: 4-minute idle window + a snapshot at 6 minutes. Not a substitute for the full run; reported here transparently.

| ID | Test | Result | Evidence |
|---|---|---|---|
| **X.1.1** (short) | Idle RSS / FD / thread growth | **INDETERMINATE** | T0 (right after launch, mid-tick): RSS=8404 KB, FD=210 (transient — `/proc` scan), Threads=4. T+~6 min (between ticks): RSS=15616 KB, FD=10, Threads=4. Δ RSS ≈ +7 MB in 6 min. Within the 1-hour <10 MB budget *if* growth plateaus, but the early-life slope alone would exceed budget if linear. **Cannot decisively pass without the full 1-hour run.** Threads stable; FD is steady-state 10 (T0=210 was a measurement artifact during a tick). |
| X.1.2 | 1-hour churn (120 spawn/kill cycles) | **SKIP** | Time budget. |
| X.1.3 | Burst spawn 100 fakes in 10 s | **SKIP** | Time budget. |
| X.2.1 | 8-hour CI nightly | **SKIP** | Not a launch gate per TEST.md. |
| X.3.1 | Post-launch canary | **N/A** | Operational. |

**Verdict:** stability is **unverified at the 1-hour gate**. Recommend running `./scripts/manual/foundations_smoke.sh` *or* a one-hour idle wall-clock test before declaring Phase 3 green.

---

## Phase 4 — Resource pressure

| ID | Test | Result | Severity | Evidence |
|---|---|---|---|---|
| RP.1 | 100% CPU host, tick latency ≤ 2× interval | **SKIP** | S2 | `stress-ng` not installed; not run. |
| RP.2 | 95% RAM host | **SKIP** | S2 | Same; would also need a VM per TEST.md. |
| RP.3 | Disk full on storage path | **SKIP** | S2 | Did not mount tmpfs; F.1.7 also untested. |
| **RP.4** | 500 concurrent processes — discovery <2 s/tick | **PASS** | S3 | Spawned 500 background `sleep 600` jobs, ran `./em --no-ui --ticks 3`. Total 2157 ms / 3 ticks = ~720 ms/tick (well under 2 s and under the ≤ 2× interval ceiling of 2000 ms). edge_monitor RSS during this load: 24420 KB. |
| RP.5 | Network down — HTTP samplers fail fast | **SKIP** | S2 | `iptables` not run; sampler timeout coverage (`dispatcher_timeout_protects_against_slow_samplers`) is the closest evidence. |

---

## Phase 5 — Performance budget

| Metric | Budget | Measured | Result |
|---|---|---|---|
| Cold start (`time --ticks 1`) | <500 ms | **165 ms** | PASS |
| RSS at idle (~50 procs visible) | <40 MB | ~8–18 MB (rises over first few minutes) | PASS |
| RSS at load (500 procs, RP.4) | <120 MB | **~24 MB** | PASS |
| CPU overhead (idle) | <2% of one core | `top` shows 0.0% (after warm-up; sampling resolution low) | PASS (qualitative) |
| CPU overhead (load) | <8% of one core | not measured precisely | UNVERIFIED |
| Tick latency p99 (idle) | <200 ms | not instrumented; per-tick wall < interval (1000 ms) by inspection | UNVERIFIED |
| Binary size | <10 MB Linux | **7.4 MB** (clippy build), 3.5 MB (strip build) | PASS |

---

## Phase 6 — Acceptance on a clean machine

Not run — no fresh VM. The entries below are best-effort static checks against the local repo.

| ID | Test | Result | Notes |
|---|---|---|---|
| A.1 | `cargo install --path .` on fresh VM | **SKIP** | Local rebuild succeeded (`cargo build --release` → `Finished release profile`). Cross-platform install path unverified. |
| A.2 | First run, no config, sensible defaults + intro | **PASS** | `./em --no-ui --ticks 1` with no config emits `no config file; using built-in defaults` and `DRY-RUN mode — no signals will be sent.` ahead of the first tick. |
| A.3 | First run on no-GPU, clear "GPU unavailable" | **PARTIAL** | See S.0.7 — message is debug-only; not visible at default log level. |
| A.4 | TUI no garbled escapes | **SKIP** | No interactive run. |
| A.5 | `q` quits cleanly | **PASS (unit-tested)** | `ui::input::tests::q_quits_in_normal_mode`, `ui::app::tests::quit_request_propagates`. |
| A.6 | README install instructions complete | **NOT VERIFIED** | Not followed end-to-end in this pass. |
| A.7 | Every flag in `--help` appears in README | **NOT VERIFIED** | Not cross-checked in this pass. |

---

## Build / lint gates (acceptance gates from HANDOFF + CLAUDE)

- `cargo build --release`: **PASS** (`Finished release profile`).
- `cargo test --release --all`: **PASS** (252 unit + 10 integration + 0 doc tests; 0 failed; ~0.4 s).
- `cargo clippy --all-targets --release -- -D warnings`: **PASS** (`Finished release profile [optimized] target(s) in 19.59s`, no warnings emitted).
- `cargo audit`: **NOT RUN** — `cargo-audit` not installed locally. README/HANDOFF acceptance still has `cargo audit clean` as an unchecked box.

---

## Aggregate counts

- Phase 0: 7 PASS / 1 FAIL (S.0.8 / S1) / 1 SKIP
- Phase 1 (mapped): 5 F.1 PASS / 6 F.1 missing-or-not-tested / 5 F.2 PASS / 5 F.3 PASS, 3 F.3 missing
- Phase 2 governor: 6 PASS / 1 FAIL (G.7 / S0) / 2 not-tested
- Phase 2 features: many SKIP (Tier 2.x, 3.x not implemented in HEAD)
- Phase 3 stability: 1 INDETERMINATE / 4 SKIP
- Phase 4: 1 PASS / 4 SKIP (no GPU/stress tooling on E1 here)
- Phase 5: 4 PASS / 2 UNVERIFIED / 1 PARTIAL
- Phase 6: mostly SKIP

---

## Launch gate per TEST.md "Realistic exit criteria"

1. **Phase 0 passes 100% on E1, E2, E3.** ❌ — S.0.8 fails on E1; E2/E3 not exercised.
2. **All Phase 1 unit + integration tests pass; property test 1000 iterations clean.** ⚠️ — `cargo test` clean, but the F.1 property test does not exist.
3. **Phase 2 happy + observability pass for every shipped feature.** ✅ for History viewer, Regression detection, Tokens/sec sampler logic. ❌ for any "shipped" claim that includes Power/Cold-start/Prometheus/fingerprint/exit-reason/vision (those are NOT shipped).
4. **Governor safety G.1–G.9 PASS; G.7 specifically must pass.** ❌ — **G.7 is unimplemented and untested. Blocker.**
5. **Phase 3 X.1 passes (1-hour idle + 1-hour churn).** ❌ — INDETERMINATE; full run not performed.
6. **Phase 4 RP at S2 or better.** ✅ for what we ran (RP.4 only).
7. **Phase 5 budgets met on E1.** ✅ for cold-start, RSS, binary size; UNVERIFIED for CPU-load and tick p99.
8. **Phase 6 acceptance: stranger can install on a fresh VM.** ❌ — not run.

**Net launch readiness:** **NOT READY.** Two blockers: G.7 (PID-reuse safety, S0) and S.0.8 (SIGTERM clean shutdown, S1). One mandatory unfinished item: Phase 3 X.1 1-hour gate.

---

## Recommended fixes / next steps (prioritized)

1. **Add `features = ["termination"]` to the `ctrlc` dependency** in `Cargo.toml`, then verify S.0.8 passes (exit 0, last-tick line shows shutdown, audit log file ends with the latest entry). Without this, **CLAUDE.md safety rule 6** (durable audit trail) is at risk.
2. **Implement G.7 PID-reuse guard.** Capture `(pid, start_ticks_since_boot)` from `/proc/<pid>/stat` field 22 when SIGTERM is sent; before sending SIGKILL, re-read the same field. If it differs (or `/proc/<pid>` is gone), drop the SIGKILL and log `KillAction::AlreadyExited` or `KillAction::PidReused`. Add a test that simulates a PID-reuse race using a fixture process.
3. **Add the F.1 property test** (`append_recent_invariant`, 1000 cases) to `src/storage/run_store.rs`.
4. **Add F.3.4 Warn-tier regression test** (e.g. 12% drop). One-line test, high signal.
5. **Run a real 1-hour X.1.1 idle gate** on E1 before tagging a release; the abbreviated 6-minute snapshot here is not a substitute.
6. **Promote the "GPU unavailable" log to one-shot info level** (S.0.7 / A.3 user visibility).
7. **Install `cargo-audit`** and re-run; HANDOFF.md still has `cargo audit clean` as an unchecked acceptance gate.

This list is bounded. It is the *minimum* to honestly say Phase 0 is green and to clear the governor's S0 gate. Feature work for Tier 2.x / 3.x continues per `latest.md` and is not in this list because it is not in the launch acceptance.
