# FEATURES.md

A snapshot of what `edge_monitor` actually does today (Phase 0 + Phase 1).
For audience and product framing see [VISION.md](VISION.md); for the build
plan see [HANDOFF.md](HANDOFF.md).

## At a glance

`edge_monitor` is a single Rust binary that watches every process on a
Linux box, decides which ones are AI workloads, tracks their resource
footprint, and (optionally) kills runaway ones — all on a one-second tick.

```
Platform  →  Classifier  →  Lifecycle  →  Governor  →  UI
/proc        keyword +       peaks +       allowlist    ratatui
sysinfo      script +        run             + dry-run    or
nvml         model           summaries      + rate       headless
                                              limit
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
  utilisation, and total GPU memory. Returns `Option<GpuSnapshot>`;
  WSL/no-GPU hosts gracefully fall through to "no VRAM data".
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
samples`.

Summaries land in three places:

- The **headless exit log** (filtered to `category.is_some()`).
- The **Completed TUI panel** (same filter).
- The **persistent run-summary JSONL** at `summary_log_path` (no filter
  — every exit is archived for forensic replay).

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

## TUI (ratatui)

[src/ui/](src/ui/) renders a six-row layout at ~10 Hz off cached state
(no blocking I/O in the render loop):

| Row | Panel | Source |
|---|---|---|
| 1 | Status bar — mode (DRY-RUN/ENFORCE), tick #, focus, filter, ARMED kill | `app::App` |
| 2 | **Vitals** — CPU/RAM/GPU aggregate | `vitals.rs` |
| 3 | **Registry / Rogues / Culprits** — three-column process row | `registry.rs`, `rogues.rs`, `culprits.rs` |
| 4 | **AI run summaries** — recent exits with model + peaks | `completed.rs` |
| 5 | **Audit** — recent governor decisions | `audit.rs` |
| 6 | Hint footer | `mod.rs` |

`?` opens an overlay help panel. `Tab` rotates focus across the three
process panels. `/` enters filter mode (case-insensitive substring
against name/model). `j`/`k` move the selection. `d` toggles dry-run.
`q` quits.

## Headless mode (`--no-ui`)

For SSH boxes, CI smoke tests, and Jetson side-loads. Logs three line
types via `tracing`:

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
  `keep_runs_per_model` (default 200)
- `[regression]` — `warn_pct` (10), `critical_pct` (25),
  `baseline_window` (10), `min_baseline_samples` (3)

`config.validate()` rejects nonsense (e.g. zero tick interval, grace
period < 1s) before the runtime starts.

## CLI

```
edge_monitor [OPTIONS] [COMMAND]
  --config <PATH>     Path to TOML config (defaults to ./edge_monitor.toml)
  --dry-run           Force dry-run regardless of policy.enforce
  --no-ui             Headless mode; one line per tick to stderr
  --ticks <N>         Headless tick budget (0 = run until killed)
  --log-level <LEVEL> trace | debug | info | warn | error
  -h, --help / -V, --version

Subcommands:
  history [MODEL] [--limit N] [--json]
                      Show recent runs from the typed run store.
                      With no model: per-model summary table.
                      With model: most-recent runs with peak metrics.
                      --json emits structured output for piping.
```

`--dry-run` is an extra safety belt on top of `policy.enforce` —
specifying it can never make the governor more aggressive than the
config says. Tracing logs route to **stderr**, so `history --json` on
stdout stays clean for `jq`.

## Run history (Tier 1.1)

[src/history.rs](src/history.rs) + [src/storage/run_store.rs](src/storage/run_store.rs)
form the backbone:

- **`RunStore`** persists every AI-classified completed run as a
  `RunRecord` (a `LifecycleSummary` plus run-id, model fingerprint
  slot, telemetry slot, exit reason, cold-start slot). On-disk:
  `<root>/runs/<YYYY-MM-DD>/run-<uuid>.json` per record + an
  append-only `index.jsonl` for O(N) startup scan.
- **CLI `history`** queries the store from any shell and renders to a
  table (default) or JSON (`--json`).
- **TUI history overlay** opens with `h` on a focused row; loads up to
  20 recent runs of the row's model. Esc / q dismisses.
- Default store path: `~/.local/share/edge_monitor`. Set
  `storage.run_store_path = ""` to disable persistence (in-memory ring
  buffer still feeds the Completed panel).

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

## Test surface

208 tests pass (198 unit + 5 history-CLI integration + 3 pipeline
integration + 2 proptest):

- Unit tests cover every classifier strategy, lifecycle peak/avg
  arithmetic, governor decisions across allowlist/blocklist/dry-run/
  rate-limit, audit-log replay, and config validation.
- [tests/pipeline_end_to_end.rs](tests/pipeline_end_to_end.rs) drives
  fake `ProcessSample` streams through the whole pipeline.
- [tests/governor_properties.rs](tests/governor_properties.rs) is a
  proptest that fuzzes governor inputs and asserts the safety
  invariants above.

`cargo clippy --all-targets -- -D warnings` is part of CI; release
binary weighs ~2.7 MB.

## What this does **not** do (Phase 2+)

Out of scope until launch is done: tegrastats, thermal zones, ROS2 node
detection, Prometheus exporter, OOM post-mortem, Intel NPU, AMD ROCm,
Hailo, web UI, Windows support, cgroup-based enforcement, rosbag
correlation. See [VISION.md](VISION.md) for why.
