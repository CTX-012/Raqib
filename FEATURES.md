# FEATURES.md

A snapshot of what `edge_monitor` actually does today. Phase 0 + Phase 1
are complete; latest.md Tier 1, Tier 2, and most of Tier 3 are shipped.
For audience and product framing see [VISION.md](VISION.md); for the
build plan and remaining queue see [latest.md](latest.md).

## At a glance

`edge_monitor` is a single Rust binary that watches every process on a
Linux box, decides which ones are AI workloads, tracks their resource
footprint and runtime telemetry (tokens/sec, fps, latency, KV cache,
GPU/CPU power, cold-load I/O, model fingerprint), regresses each new
run against a rolling baseline, and (optionally) kills runaways — all
on a one-second tick.

```
Platform  →  Classifier  →  Lifecycle  →  Telemetry  →  Governor  →  UI
/proc        keyword +       peaks +       vLLM /        allowlist    ratatui
sysinfo      script +        run           llama.cpp /     + dry-run    or
nvml         model           summaries     Ollama /        + rate       headless
rapl                                       stdout +        limit
                                           probe socket
                                           +
                                           cold-load +
                                           power +
                                           fingerprint +
                                           exit classify
```

---

## Process discovery (Platform layer)

- **Linux backend** ([src/platform/linux_proc.rs](src/platform/linux_proc.rs))
  reads `/proc/<pid>/{stat,status,cmdline,environ}` once per tick and
  cross-references `sysinfo` for CPU and RSS. CPU% is computed from
  `utime + stime` deltas across two ticks (so the first tick reports 0%
  by design).
- **NVIDIA backend** ([src/platform/gpu_nvidia.rs](src/platform/gpu_nvidia.rs))
  attaches to NVML when available and reports per-process VRAM,
  utilisation, total GPU memory, **per-board power (W)** and
  **temperature (°C)**. Returns `Option<GpuSnapshot>`; WSL/no-GPU hosts
  gracefully fall through to "no VRAM data" without panicking.
- Permission-denied on `/proc/<pid>/environ` does **not** drop the
  process — root daemons and PID 1 still appear in the snapshot with an
  empty environ map.

## Classifier (4 strategies, in priority order)

Pure logic, no hardware. Lives in [src/classifier/](src/classifier/).

1. **Script sniffing** ([script_sniff.rs](src/classifier/script_sniff.rs))
   — when the process runs a `.py`/`.sh` script, the file is read and
   scanned for AI-constructor literals. The model name is lifted out of
   the source, even when the process name is just `python`:
   - `YOLO("yolov8n.pt")` → model `yolov8n`
   - `Llama(model_path="/models/phi3-mini.gguf")` → model `phi3-mini`
   - `AutoModelForCausalLM.from_pretrained("meta-llama/Llama-3-8B")`
     → model `meta-llama/Llama-3-8B`
   - `whisper.load_model("small.en")` → model `small.en`
2. **Argv path extraction** ([model_extract.rs](src/classifier/model_extract.rs))
   — scans cmdline tokens for `.gguf` / `.safetensors` / `.pt` / `.bin`
   paths in `--flag value`, `--flag=value`, and bare-token forms. Hit
   classifies the process as `Inference` and pulls the model basename
   for display.
3. **Strong env vars** — same module checks `MODEL_PATH`,
   `LLAMA_MODEL_PATH`, `GGUF_MODEL`, `OLLAMA_MODELS`. Path-valued vars
   yield a model name; directory-valued vars (`OLLAMA_MODELS`) classify
   the process but don't pin a model.
4. **Keyword match** ([keyword_match.rs](src/classifier/keyword_match.rs))
   — a static table of names and cmdline substrings (`llama-server`,
   `vllm`, `tritonserver`, `torchrun`, `deepspeed`, `huggingface-cli`,
   `transformers`, `pytorch`, `onnxruntime`, `comfyui`, `whisper`,
   `stable-diffusion`, `gpt`, `llm`, …). Short keywords are word-boundary
   matched so `ai` never fires on `mail`.

**Categories** (`AICategory`): `Inference`, `Training`, `ModelDownload`,
`Framework`, `NotAi`.

## Lifecycle + run summaries

[src/lifecycle/](src/lifecycle/) maintains a per-PID record from spawn
to exit and folds each tick's reading into a rolling `ResourceStats`:

- `cpu_sum_pct`, `cpu_peak_pct`, `rss_peak_bytes`, `vram_peak_bytes`,
  `sample_count` (and derived `avg_cpu_pct`).
- `model_name` is *sticky*: once the classifier resolves a model, later
  ticks that lose the signal (e.g. exec into a wrapper) do not erase it.

When a process exits, a `LifecycleSummary` is emitted carrying:
`pid, name, category, model_name, spawn_time, exit_time, uptime_secs,
exit_code, signal, avg_cpu_pct, peak_cpu_pct, peak_rss_mb, peak_vram_mb,
samples`. AI-classified summaries are wrapped into a typed
[`RunRecord`](src/storage/run_store.rs) (run_id + telemetry +
fingerprint + cold-start stats + classified `ExitReason`) and persisted
to the run store.

Summaries land in three places:

- The **headless exit log** (filtered to `category.is_some()`).
- The **Completed TUI panel** (same filter).
- The **persistent run-summary JSONL** at `summary_log_path` (no filter
  — every exit is archived for forensic replay).

## AI-runtime telemetry (Tier 1.2 + 2.x + 3.x)

[src/telemetry/](src/telemetry/) is a Tokio-based dispatcher that
samples per-PID metrics in parallel with the 1 Hz tick loop:

- **vLLM Prometheus sampler** (1.2a, [vllm_prometheus.rs](src/telemetry/samplers/vllm_prometheus.rs))
  — detects `vllm serve` / `vllm.entrypoints.*` / `python -m vllm` /
  `VLLM_*` env, scrapes `http://127.0.0.1:<port>/metrics` (port from
  `--port`, default 8000) with a 500 ms timeout. Maps
  `vllm:avg_generation_throughput_toks_per_s` → `tokens_per_sec`,
  `vllm:gpu_cache_usage_perc` → `kv_cache_pct` (×100), and
  `vllm:num_requests_running` → `concurrent_requests`. Tier 3.3 also
  pulls `vllm:num_preemptions_total` for KV-cache eviction counts.
- **llama.cpp server sampler** (1.2b, [llama_cpp_server.rs](src/telemetry/samplers/llama_cpp_server.rs))
  — detects `llama-server`, scrapes `:<port>/metrics` (default 8080),
  derives tok/s from the monotonic `llama_server_n_decode_total`
  counter when no direct gauge is exposed, and maps
  `llama_server_n_busy_slots` → `concurrent_requests` and
  `llama_server_kv_cache_usage` (0..1) → `kv_cache_pct` (×100).
- **Ollama API sampler** (1.2c, [ollama_api.rs](src/telemetry/samplers/ollama_api.rs))
  — confirms which model is loaded via `/api/ps`; the dispatcher
  promotes the model name onto `RunRecord` even when the classifier
  saw only `ollama runner`.
- **Stdout regex parser** (1.2d, [stdout_parser.rs](src/telemetry/samplers/stdout_parser.rs))
  — pure-function `parse_line()` extracts tokens/sec, fps, and
  latency from llama.cpp `eval time` lines, vLLM `Avg generation
  throughput` lines, Ollama `eval rate: NN.NN tokens/s` lines (the
  fall-through path documented for Tier 1.2c — added by [A-5] to
  close T2's V1 ground-truth gap; explicitly does NOT match Ollama's
  `prompt eval rate:` line which reports a separate quantity), and
  Ultralytics `Speed: ...preprocess, ...inference, ...postprocess`
  lines. Strict — refuses partial matches so noise lines never
  produce 0.0 readings.
- **`edge_monitor exec` wrapper** (1.2d, [exec_wrapper.rs](src/exec_wrapper.rs))
  — `edge_monitor exec [--name LABEL] -- COMMAND ARGS...` forks
  `COMMAND` with piped stdio, tees both streams to your terminal AND
  through the stdout parser, and writes a `RunRecord` on exit. Ctrl-C
  forwards SIGINT to the child; a second Ctrl-C hard-exits 130 so a
  stuck child can't trap the user. Stderr tail (capped at 64 lines)
  flows into the Tier 3.5 exit-reason context.
- **Vision probe Unix socket** (3.6, [vision_probe.rs](src/telemetry/vision_probe.rs))
  — listens on the path configured by `[telemetry] vision_probe_socket`
  for line-delimited `{"pid": ..., "frame_at_ns": ...}` JSON; each
  event aggregates into a per-PID rolling 1-second window and
  instantaneous fps flows into the accumulator. Disabled when the
  config value is empty.

The dispatcher applies a per-sample timeout (default 1 s) so a hung
HTTP scrape can't pile up; a panicking sampler is isolated so the
runtime keeps ticking. Frames flow through an unbounded mpsc channel
into the per-PID `TelemetryAccumulator` and are merged onto the
`RunRecord` at exit.

### Cold-load detection (Tier 2.2)

[src/telemetry/cold_load.rs](src/telemetry/cold_load.rs) watches
`/proc/<pid>/io` `read_bytes` per AI process. Heuristic: 16 MiB floor
plus 2 consecutive ≤1 MiB/s ticks ⇒ load complete (hard timeout 60 s
for streaming inference). The resulting `ColdStartStats`
(`duration_seconds`, `bytes_read`, `avg_throughput_mbps`,
`peak_throughput_mbps`) lands on `RunRecord.cold_start`.

### Cold-start vs steady-state separation (Tier 3.2)

The moment cold-load completes, `TelemetryAccumulator::mark_steady_state(pid)`
flips a watermark; frames recorded after that contribute to BOTH the
overall sums AND new `_steady` aggregates: `tokens_per_sec_avg_steady`,
`fps_avg_steady`, `gpu_watts_avg_steady`. History comparison should
prefer the steady value because it ignores model-load warm-up noise.

### Power & thermals (Tier 2.1)

- **NVML** — per-board GPU watts and °C from
  `nvmlDeviceGetPowerUsage` and `nvmlDeviceGetTemperature(GPU)`. NVML
  errors swallow into `None` rather than failing the whole read.
- **RAPL** ([src/telemetry/rapl.rs](src/telemetry/rapl.rs)) — sums
  package energy across `intel-rapl:N` sysfs entries; Δ-based wattage
  with wraparound handled via `max_energy_range_uj`. Permission-gated
  hosts emit a single warn log then degrade to `None` watts (no
  log spam).
- `Dispatcher::record_system_power` divides totals across AI processes
  to apportion. `RunMetrics` carries `gpu_watts_avg`, `gpu_watts_peak`,
  `cpu_watts_avg`, and `energy_joules_total` (trapezoidal integration
  of the wattage stream). `tegrastats` for Jetson is not yet wired —
  see "remaining gaps" below.

### KV cache pressure (Tier 3.3)

`RunMetrics` gains `kv_cache_avg_pct` and `kv_cache_evictions_total`.
The accumulator tracks per-PID KV avg via sum/count and evictions via
first/last counter delta; counter resets snap forward so the delta
stays non-negative. The TUI registry row appends a `KV NN%` segment
that turns red+bold at ≥80%; the history overlay flags runs whose
peak hit ≥99.5% with a `KV!` badge so saturation events are visible
at a glance.

### Model fingerprinting (Tier 3.1)

[src/fingerprint.rs](src/fingerprint.rs) hashes
`len_le_bytes || head[0..1MiB] || tail[len-64KiB..]` into SHA-256,
prefixed `sha256-head1m-tail64k:`. Partial by design — a full hash of
a 40 GB Llama-70B would dwarf the 1 s tick. Head+tail differentiates
quantization variants and distinct fine-tunes in <50 ms even on slow
disks. Documented collision: middle-only modifications share the same
fingerprint (asserted by a test). Cached at the path configured by
`storage.fingerprint_cache` (keyed on `(dev, inode, mtime_secs, len)`)
so repeat runs of the same weights file are instant.

### Exit-reason classification (Tier 3.5)

[src/exit_classify.rs](src/exit_classify.rs) is a pure-function
classifier on top of `ExitReason::from_summary`. Variants:
`CleanExit`, `UserSignal { signal }`, `GovernorKill { reason }`,
`Segfault`, `OutOfMemory { ram, vram }`, `CudaError { last_msg }`,
`Crash { exit_code }`, `Unknown`. Precedence (highest first):
governor → SIGSEGV → OOM (kernel via dmesg PID match, OR CUDA via
stderr) → CUDA error → bare signal / exit code / Unknown. The dmesg
match keys on `process <PID>` / `pid=<PID>` patterns ONLY (never on
truncated process names — kernel truncates to 15 chars and `python`
is shared by every ML workload). `read_recent_kernel_log(secs)` wraps
`journalctl -k --since=-Ns` and returns `Vec::new()` on any failure
so the classifier degrades gracefully on hosts without journald.
`history::format_exit_short` renders compact tokens (`segfault`,
`oom(ram)`, `oom(vram)`, `oom(ram+vram)`, `cuda_error`).

## Governor (decide → act)

[src/governor/](src/governor/) is split into four files:

- **policy.rs** — per-process decision. Inputs: process category,
  allowlist, blocklist, `default_ai_action`. Output: `PolicyAction =
  Allow | Kill`.
- **executor.rs** — turns a `Kill` decision into a signal. SIGTERM
  first, wait `sigterm_grace_secs`, then SIGKILL. Returns
  `KillAction::{SignalTermSent, SignalKillSent, DryRunTermWould,
  DryRunKillWould, Whitelisted, AlreadyExited, RateLimited, Skipped}`.
- **audit.rs** — every kill (or would-kill) is appended to
  `audit_log_path` as JSONL, with a `replay()` helper for post-mortem.
- **manual.rs** — operator-driven kill from the TUI. Allowlist still
  applies; the UI prompts twice before sending a real signal.

**Rate limit:** at most `rate_limit_max_kills` (default 3) automated
kills inside a `rate_limit_window_secs` (default 60) sliding window.
Dry-run "would-kill" decisions do **not** consume budget.

**Hard safety invariants** (CLAUDE.md, never violated):

1. Allowlisted processes are never killed by automated policy.
2. Dry-run never emits a signal — only logs.
3. Dry-run is the default in shipped config.
4. SIGTERM always precedes SIGKILL with a configurable grace period.
5. Every kill (real or would-be) hits the JSONL audit trail.

## Manual kill

In the TUI, `k` arms a kill against the focused PID, `k` again confirms.
The status bar shows an `ARMED kill PID=…` banner while a kill is
pending. Allowlist is still respected; an explicit confirm overrides it
only for processes outside the allowlist. Manual kills also flow through
the audit log with `KillSource::Manual`.

## Persistent storage

[src/storage/log_store.rs](src/storage/log_store.rs) is a thin JSONL
appender shared by the audit log and the run-summary log. Both files are
append-only, line-delimited, and survive restarts. Either log is
disabled by setting its config path to `""` (the default).

[src/storage/run_store.rs](src/storage/run_store.rs) is the typed run
store: `<root>/runs/<YYYY-MM-DD>/run-<uuid>.json` per record + an
append-only `index.jsonl` for O(N) startup scan. Crash-safe: the record
file is fsynced before the index entry is appended, so a partial write
leaves an orphaned file (recoverable) rather than a dangling index
pointer.

## TUI (ratatui)

[src/ui/](src/ui/) renders a six-row layout at ~10 Hz off cached state
(no blocking I/O in the render loop):

| Row | Panel | Source |
|---|---|---|
| 1 | Status bar — mode (DRY-RUN/ENFORCE), tick #, focus, filter, ARMED kill | `app::App` |
| 2 | **Vitals** — CPU/RAM/GPU aggregate (incl. GPU watts and temp) | `vitals.rs` |
| 3 | **Registry / Rogues / Culprits** — three-column process row, with `KV NN%` segment when present | `registry.rs`, `rogues.rs`, `culprits.rs` |
| 4 | **AI run summaries** — recent exits with model + peaks + classified exit | `completed.rs` |
| 5 | **Audit (kills + regressions)** — interleaved newest-first, critical regressions red, warnings yellow | `audit.rs` |
| 6 | Hint footer | `mod.rs` |

`?` opens an overlay help panel. `Tab` rotates focus across the three
process panels. `/` enters filter mode (case-insensitive substring
against name/model). `j`/`k` move the selection. `h` opens the
**history overlay** for the focused row's model (last 20 runs, with
`KV!` badge on saturation events). `d` toggles dry-run. `q` quits.

## Headless mode (`--no-ui`)

For SSH boxes, CI smoke tests, and Jetson side-loads. Logs three line
types via `tracing` (stderr — stdout reserved for subcommand JSON):

- `tick` — per-tick aggregate (`tick=N ai_processes=K exits=M`)
- `ai-process` — one line per live AI process with pid/name/category/
  model/cpu_pct/rss_mb/vram
- `exit` — one line per **AI** exit (non-AI exits are filtered out of
  this stream but still hit the JSONL summary log) with pid/name/
  category/model/uptime + avg/peak CPU + peak RSS + peak VRAM + samples

`--ticks N` caps the run for CI; `--ticks 0` (default) runs until SIGINT.

## Configuration (TOML)

Loaded from `--config <path>` or `./edge_monitor.toml`; falls back to
built-in safe defaults. Schema (see
[edge_monitor.toml.example](edge_monitor.toml.example) and
[docs/configuration.md](docs/configuration.md)):

- `[runtime]` — `tick_interval_ms`, `render_interval_ms`,
  `completed_history`, `audit_history`, `audit_log_path`,
  `summary_log_path`
- `[policy]` — `allowlist`, `blocklist`, `default_ai_action`,
  `sigterm_grace_secs`, `enforce`, `rate_limit_max_kills`,
  `rate_limit_window_secs`
- `[storage]` — `run_store_path` (default
  `~/.local/share/edge_monitor`), `fingerprint_cache`,
  `keep_runs_per_model`
- `[regression]` — `warn_pct` (10), `critical_pct` (25),
  `baseline_window` (10), `min_baseline_samples` (3)
- `[telemetry]` — `vllm_scrape` (true), `llamacpp_scrape` (true),
  `ollama_api` (true), `prometheus_bind` (empty disables the Tier 2.3
  exporter), `vision_probe_socket` (empty disables the Tier 3.6
  vision probe)

`config.validate()` rejects nonsense (e.g. zero tick interval, grace
period < 1s, `critical_pct < warn_pct`) before the runtime starts.

## CLI

```
edge_monitor [OPTIONS] [COMMAND]
  --config <PATH>     Path to TOML config (defaults to ./edge_monitor.toml)
  --dry-run           Force dry-run regardless of policy.enforce
  --no-ui             Headless mode; one line per tick to stderr
  --ticks <N>         Headless tick budget (0 = run until killed)
  --log-level <LEVEL> trace | debug | info | warn | error
  --log-format <FMT>  text | json
  -h, --help / -V, --version

Subcommands:
  history [MODEL] [--limit N] [--json]
                      Show recent runs from the typed run store.
                      With no model: per-model summary table.
                      With model: most-recent runs with peak metrics.
                      --json emits structured output for piping.

  compare MODEL [MODEL ...] [--runs N] [--json]
                      Side-by-side baseline comparison across models
                      (latest.md Tier 3.7). Folds the most recent N
                      records per model into a Foundation-C Baseline,
                      prints tok/s avg ± stddev, peak VRAM, W/token,
                      cold load.

  exec [--name LABEL] -- COMMAND ARGS...
                      Run a workload under instrumentation
                      (latest.md Tier 1.2d). Tees stdout/stderr to
                      your terminal AND through the stdout regex
                      parser, writes a RunRecord on exit.
```

`--dry-run` is an extra safety belt on top of `policy.enforce` —
specifying it can never make the governor more aggressive than the
config says. Tracing logs route to **stderr**, so `history --json` /
`compare --json` on stdout stay clean for `jq`.

## Prometheus exporter (Tier 2.3)

[src/telemetry/exporter.rs](src/telemetry/exporter.rs) exposes a
hand-rolled `text/plain; version=0.0.4` endpoint at
`[telemetry] prometheus_bind` (e.g. `127.0.0.1:9472`). Disabled by
default. Per-request 8 KiB header cap + 5 s read timeout protect
against slowloris / memory-exhaust scrapes. Output is sorted by label
so golden-file diffing and Grafana caching are stable. Metrics:

- `edge_monitor_processes_total{category}` — gauge.
- `edge_monitor_run_tokens_per_sec{model,pid}` — gauge.
- `edge_monitor_run_fps{model,pid}` — gauge.
- `edge_monitor_run_vram_bytes{model,pid}` — gauge.
- `edge_monitor_run_gpu_watts{model,pid}` — gauge.
- `edge_monitor_run_cpu_watts{model,pid}` — gauge.
- `edge_monitor_governor_kills_total{reason}` — counter.
- `edge_monitor_regressions_total{model,metric}` — counter.
- `edge_monitor_tick_count` — counter.

## Run history (Tier 1.1)

[src/history.rs](src/history.rs) + the run store form the backbone:

- **CLI `history`** queries the store from any shell and renders to a
  table (default) or JSON (`--json`).
- **TUI history overlay** opens with `h` on a focused row; loads up to
  20 recent runs of the row's model. Esc / q dismisses. Saturation
  events (KV peak ≥99.5%) carry a `KV!` badge.
- Default store path: `~/.local/share/edge_monitor`. Set
  `storage.run_store_path = ""` to disable persistence (the in-memory
  ring buffer still feeds the Completed panel).

## Baseline + regression detection (Tier 1.3)

[src/analysis/compare.rs](src/analysis/compare.rs) computes per-metric
mean/stddev across a configurable rolling window and flags new runs
that exceed warn (default 10%) / critical (default 25%) thresholds.
Refuses to flag anything with a baseline of <3 samples. Direction-
aware — a faster `tokens_per_sec_avg` is never a regression.

The runtime's exit hook ([src/runtime.rs](src/runtime.rs)
`check_regressions`) runs after every AI process exits:

- Pulls the most recent `[regression] baseline_window` runs of the
  exiting model from `RunStore` and excludes the in-flight record.
- Calls `detect_regressions_with()` with the configured thresholds.
- Emits a `tracing::warn!` per regression with structured fields, so
  headless operators see the alert in stderr.
- Pushes a `RegressionEvent` onto `RuntimeState.regressions` (bounded
  by `runtime.audit_history`).
- The Audit TUI panel renders kills + regression events interleaved
  by timestamp; critical regressions are red, warnings yellow.

Configurable in `[regression]`: `warn_pct`, `critical_pct`,
`baseline_window`, `min_baseline_samples`.

## Side-by-side comparison (Tier 3.7)

`edge_monitor compare phi3-mini llama-3.1-8b --runs 10` folds the
most recent N records per model into a Foundation-C `Baseline` and
prints them in side-by-side columns:

```
              phi3-mini (n=5)        llama-3.1-8b (n=10)
tok/s avg     38.4 ± 2.1            21.7 ± 0.8
peak VRAM     4.0 GB                15.0 GB
W/token       0.082                 0.341
cold load     3.2 s                 18.6 s
```

W/token is computed mean-of-ratios (per-record `energy_joules_total /
tokens_total`, averaged across the window) so a single 1000-token run
with 100 J doesn't outweigh five 100-token runs. Returns `None` when
neither field is populated. `--json` emits a `Vec<ComparisonColumn>`
for piping into `jq`.

## Test surface

`cargo test --release` reports **327 lib unit + 3 concurrent-request
e2e + 1 expect-rule guard + 3 governor pid-reuse + 2 governor
proptest + 5 history-CLI + 2 log-format + 3 pipeline + 1 SIGTERM
clean-shutdown = 347 tests** today, all passing. (Recently added:
the expect-rule guard enforces CLAUDE.md's `expect()` allowlist
[A-1]; `--log-format json` integration tests [A-2]; SIGTERM
clean-shutdown integration test [A-3]; concurrent-request awareness
end-to-end coverage [A-4]; Ollama `eval rate:` regex unit tests
[A-5]; pid-reuse covers the governor's PID-recycling safety; the
governor proptest fuzzes the safety invariants; new run-store
property test (1000 cases) and write-rejection / robust-baseline
tests landed via [C-2]/[C-4]/[C-5].)

`cargo clippy --all-targets -- -D warnings` is part of CI. The
release binary now weighs ~7.4 MB (was ~2.7 MB pre-Tier-1.2) — the
growth comes from `tokio` (rt-multi-thread + time + sync + io-util +
process + net) and `reqwest` (rustls-tls + http2). Trimming back
under the 5 MB budget (cargo feature to disable HTTP samplers
entirely, or switching to `native-tls`) is deferred until v0.1.0 is
out the door.

## Remaining gaps for v0.1.0

Tier 3.4 — concurrent-request awareness — has landed [A-4]:
`vllm:num_requests_running` and `vllm:num_requests_waiting` are
parsed, `RunMetrics` carries `concurrent_requests_avg` (time-
weighted), `concurrent_requests_peak`, and
`concurrent_requests_waiting_peak`. The data is queryable via
`history --json` today. The remaining loose end is the per-row text
rendering called out by `latest.md` Tier 3.4 spec example
("`#14  serving 8 concurrent (peak)  →  20.1 tok/s/req · 161 tok/s
aggregate`") in `src/history.rs`; flagged in `BUILDER_STATUS.md`
cross-builder requests, ownership not yet assigned.

Two known wiring gaps surfaced by the smoke-script audit (filed in
`BUILDER_STATUS.md` cross-builder requests):

- **Tier 3.6 vision probe socket** — `Dispatcher::enable_vision_probe`
  exists but `Runtime::new` never calls it, so
  `[telemetry] vision_probe_socket = "..."` is currently a no-op.
  Fix is one line in `runtime.rs` next to the existing
  `enable_exporter` call.
- **Tier 3.2 cold-start vs steady-state via `exec`** — the cold-load
  tracker only runs from the headless tick loop; `edge_monitor exec`
  doesn't plumb `record_disk_io(...)`, so a workload run under the
  exec wrapper cannot transition to steady-state. Either plumb it
  or scope Tier 3.2 to headless mode in `latest.md`.

One UX rough edge surfaced by Tester 2's V3 walkthrough:

- **Headless no-GPU visibility** (S3) — when NVML init fails on a
  no-GPU host, the failure is logged at `DEBUG` level only; default
  `info` shows nothing. The TUI displays the message correctly;
  headless users see silent operation. Fix is a one-line
  log-level bump in the NVML init path.

Genuinely off the roadmap (anti-goals from `latest.md` + `CLAUDE.md`):
ROS2 node detection, Intel NPU, AMD ROCm, Hailo, web UI, Windows
support, cgroup-based enforcement, rosbag correlation, cloud cost
tracking, ML-based anomaly detection, automatic regression
remediation. `tegrastats` for Jetson is named in latest.md Tier 2.1
as in-scope but not yet implemented; the Jetson AGX Orin target has
not yet been validated end-to-end.
