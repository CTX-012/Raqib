# CLAUDE.md

> Read this every session. If it conflicts with what the user says in the
> current session, surface the conflict — do not silently drift.
>
> For audience/vision: see [VISION.md](VISION.md).
> For detailed build plan per module: see [HANDOFF.md](HANDOFF.md).

## What this project is

Model-aware resource monitor and governor for edge AI workloads.
Linux-first. Target: Ubuntu 22.04+ and JetPack 6 on Jetson Orin.

## Scope (v1 / Phase 0)

- `/proc` + sysinfo process/CPU/RAM/network sampling
- NVML for GPU utilization, VRAM, per-process VRAM
- Classifier: keyword match, model path extraction, script sniffing,
  AI category assignment
- Governor: allowlist, dry-run default, SIGTERM→grace→SIGKILL, audit log,
  rate limit
- Manual kill by PID (respects allowlist with confirm)
- Process run summary on termination
- ratatui TUI (built last)

## Scope (post-Phase-1, follows latest.md)

The `latest.md` spec at the repo root is now the authoritative roadmap.
Foundations A (RunStore) + C (baseline / regression) and Tier 1.1
(history viewer) are implemented; Tier 1.2 / 1.3 / 2.x / 3.x are the
queue. Read `latest.md` before adding or removing scope.

## Out of scope (still deferred unless `latest.md` adds them)

ROS2 node detection, Intel NPU, AMD ROCm, Hailo, web UI, Windows
support, cgroup-based enforcement, rosbag correlation. Anti-goals at
the bottom of `latest.md` are also still off the table:

- Web UI (Prometheus + Grafana is the answer once 2.3 ships).
- Cloud cost tracking.
- Multi-host fleet aggregation.
- ML-based anomaly detection.
- Automatic regression remediation.

If the user asks for an out-of-scope item, push back: "That's not in
`latest.md`. Confirm you want to expand scope, or defer."

Items previously off the list that **`latest.md` brings into scope** —
do *not* push back on these any more, just implement them in tier
order: tegrastats (2.1), thermal zones (2.1), Prometheus exporter
(2.3), OOM post-mortem (3.5).

## Module build order (strict, no parallel)

1. Classifier — pure logic, no hardware
2. Platform layer — `/proc` + sysinfo
3. NVML GPU backend
4. Lifecycle + run summaries
5. Governor (dry-run first)
6. Manual kill wiring
7. ratatui TUI
8. main.rs wiring + CLI + config

Each module must:

- Have passing unit tests before the next starts
- Pass `cargo clippy --all-targets -- -D warnings`
- Have no `unwrap()` or `expect()` outside tests

## Architecture

```
Platform → Classifier → Lifecycle → Governor → UI
            (annotate)    (track)    (decide)   (render)
```

One tick per second by default. UI renders at 10 Hz with cached data.

`Platform` is an enum, not a trait — we have 2 backends in v1 (Linux,
Jetson). Refactor to `Box<dyn PlatformMetrics>` at 4+ backends, not before.

## Coding conventions

- `thiserror` for typed error enums per module
- `anyhow` for error propagation at `main.rs` boundary only
- `tracing` for logs — never `println!` or `eprintln!` in library code
- Comments explain WHY, not WHAT
- After any non-trivial block, the code must make one of these visible:
  key decision, baked-in assumption, or what breaks if the assumption is
  wrong
- Every external dependency call (file I/O, NVML, signals) must have
  explicit error handling — no silent `.ok()` swallowing
- No blocking I/O in the tick loop
- No `std::thread::sleep` — use `std::time::Instant` and elapsed checks,
  or `mpsc::Receiver::recv_timeout` as the TUI/headless park points

## Safety rules (never violate)

1. Governor never kills allowlisted processes via automated policy
2. Governor in dry-run NEVER emits kill signals — only logs
3. Dry-run is the default in config templates
4. SIGTERM before SIGKILL, always, with a configurable grace period
5. Rate limit: max 3 automated kills per 60-second window
6. Every kill logged with reason, PID, name, model, timestamp to a
   persistent JSONL audit trail

## Environment caveats

- Primary dev env: WSL Ubuntu. Limited — no real GPU, no thermal, fake
  `/sys`. Don't trust WSL for GPU/thermal feature verification.
- Real test target: Jetson AGX Orin via SSH. Any feature touching
  hardware must be tested on Orin before merge.
- NVML may fail to initialize on WSL. Code must handle
  `Option<GpuSnapshot>` everywhere. Never panic on missing NVML.

## When the user pushes for shortcuts

The user's system prompt says they want mentor-style pushback, not
convenience. If asked to:

- Skip tests → refuse, write tests first
- Parallelize modules → refuse, cite build order above
- Stub hardware calls "for later" → refuse, handle errors now
- Add a Phase 2 feature → push back, cite [VISION.md](VISION.md) guardrails
- Ship without dry-run default → refuse, cite safety rules

## Session rhythm

- Start of session: re-read this file + current module section in
  [HANDOFF.md](HANDOFF.md)
- End of each non-trivial change: run `cargo test && cargo clippy`
- Before switching modules: all tests for current module green, doc
  comments written, section in HANDOFF.md marked complete
