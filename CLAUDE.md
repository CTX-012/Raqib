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

## Out of scope (defer to Phase 2+)

tegrastats, thermal zones, ROS2 node detection, Prometheus exporter,
OOM post-mortem, Intel NPU, AMD ROCm, Hailo, web UI, Windows support,
cgroup-based enforcement, rosbag correlation.

If the user asks for these, push back: "That's Phase 2. Confirm you want
to expand Phase 0 scope, or defer."

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
