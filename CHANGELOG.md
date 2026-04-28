# Changelog

All notable changes to `edge_monitor` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project aims to adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once `v1.0.0` is tagged. Until then, minor versions may include breaking changes.

## [Unreleased]

### Added
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
- Tracing logs now route to **stderr** (was stdout) so subcommand JSON
  output (`history --json`) on stdout stays clean for piping into `jq`.

### Notes
- Developed on WSL Ubuntu; NVML returns `None` gracefully without GPU
  passthrough. Real target (Jetson AGX Orin) not yet validated end-to-end.
- 191 unit + 5 history-CLI integration + 3 pipeline integration + 2
  proptest tests pass (was 168; +13 foundations, +6 history unit,
  +4 config edge cases, +5 history-CLI integration). `cargo clippy
  --all-targets -- -D warnings` clean.
- No release artifact yet. `v0.1.0` will be tagged once Phase 1 launch
  checklist (CI, demo GIF, `.deb`, crates.io name reservation) is complete.

[Unreleased]: https://github.com/Mohaaxa/edge_monitor/compare/HEAD...HEAD
