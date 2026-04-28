# Changelog

All notable changes to `edge_monitor` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project aims to adhere to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once `v1.0.0` is tagged. Until then, minor versions may include breaking changes.

## [Unreleased]

### Added
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

### Notes
- Developed on WSL Ubuntu; NVML returns `None` gracefully without GPU
  passthrough. Real target (Jetson AGX Orin) not yet validated end-to-end.
- 168 unit + integration tests pass. `cargo clippy --all-targets -- -D warnings`
  clean.
- No release artifact yet. `v0.1.0` will be tagged once Phase 1 launch
  checklist (CI, demo GIF, `.deb`, crates.io name reservation) is complete.

[Unreleased]: https://github.com/Mohaaxa/edge_monitor/compare/HEAD...HEAD
