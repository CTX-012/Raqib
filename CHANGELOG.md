# Changelog

All notable changes to `edge_monitor` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project aims to adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once `v1.0.0` is tagged. Until then, minor versions may include breaking changes.

## [Unreleased]

### Added
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
- 313 lib unit + 1 expect-rule guard + 3 governor pid-reuse + 2
  governor proptest + 5 history-CLI + 3 pipeline = 327 tests pass on
  release (`cargo test --release`).
- No release artifact yet. `v0.1.0` will be tagged once Phase 1 launch
  checklist (CI, demo GIF, `.deb`, crates.io name reservation) is complete.

[Unreleased]: https://github.com/Mohaaxa/edge_monitor/compare/HEAD...HEAD
