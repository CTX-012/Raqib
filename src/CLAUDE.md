## Module build order (strict)

1. Classifier (pure logic, no hardware) — Module 1
2. Platform layer (/proc + sysinfo) — Module 2
3. NVML GPU backend — Module 3
4. Lifecycle + summaries — Module 4
5. Governor (dry-run first) — Module 5
6. Manual kill wiring — Module 6
7. ratatui UI — Module 7
8. main.rs wiring — Module 8

Do not skip ahead. Do not work on multiple modules in parallel.
Each module must have tests passing before the next one starts.
# edge_monitor

Model-aware resource monitor and governor for edge AI workloads.
Target: Linux (Ubuntu 22.04+, JetPack 6). Linux-first; Windows deferred.

## Scope (v1)
- /proc + sysinfo process/CPU/RAM/network sampling
- NVML for GPU%, VRAM, per-process VRAM
- Classifier: keyword match, model path extraction, script sniffing, AI category
- Governor: allowlist, dry-run default, SIGTERM→SIGKILL, audit log
- Manual kill by serial ID
- Process summary on termination
- ratatui TUI (last)

## Out of scope (defer)
tegrastats, thermal, ROS2 nodes, Prometheus, OOM post-mortem, 
Intel NPU, AMD ROCm, web UI, Windows.

## Architecture
Platform → Classifier → Lifecycle → Governor → UI, one tick loop.
PlatformMetrics is an enum for now (2 backends), refactor to trait at 4+.
Governor is dry-run by default. Allowlist-first. SIGTERM, wait, SIGKILL.
Manual kill bypasses automated policy but still respects allowlist 
(with explicit override confirm).

## Coding conventions
- thiserror for typed errors per module; anyhow at main boundary
- tracing for logs, not println/eprintln
- every non-trivial function has a unit test
- comments explain WHY, not WHAT
- every external dep has timeout + error handling

## Known ground truth
- Dev env: WSL Ubuntu (limited — no real GPU, no thermal, fake /sys)
- Real test target: Jetson AGX Orin via SSH
- NVML may fail to init on WSL; handle Option<GpuSnapshot> everywhere

## Current status
Phase 0 / Module 1: classifier port from legacy Windows code.
Next: platform/linux_proc.rs producing ProcessSample.