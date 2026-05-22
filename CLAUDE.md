# CLAUDE.md

> Read this every session. If it conflicts with what the user (or the
> orchestrator running a multi-agent session) says, surface the conflict —
> do not silently drift.
>
> **Locked source of truth: [DESIGN_HANDOFF.md](DESIGN_HANDOFF.md).**
> UX_CONTRACT.md v0.3 lives inside it (§0–§15). The Linux implementation
> plan (L1–L26) lives there too. Read both before editing user-visible
> code.

## What this project is

Model-aware resource monitor and governor for edge AI workloads.
Linux-first. Target: Ubuntu 22.04+ and JetPack 6 on Jetson Orin.

A sibling Windows binary lives in a separate repo and shares the same
UX contract; this repo is the Linux reference implementation.

## Scope

Current scope is whatever the locked UX_CONTRACT.md v0.3 (in
`DESIGN_HANDOFF.md`) describes. Out-of-scope items are listed in
UX_CONTRACT.md §0 — push back on requests for them rather than
silently expanding scope.

## Multi-agent workflow

This repo is currently being driven by a parallel-agent workflow
coordinated by an orchestrator:

- **Agent A** owns the shared `ux_contract` crate at `~/ux_contract`.
- **Agent B** (this repo) consumes `ux_contract` via path dependency
  and ships the Linux L1–L26 plan, one PR per row.
- **Agent C** owns the Windows repo and the W1–W49 plan.

**No UX changes without a contract amendment.** If implementing a row
reveals a string template, alert ID, threshold, action, theme, or sizing
constant that v0.3.0 of `ux_contract` does not provide, do **not** edit
`~/ux_contract` from this repo — file a "Contract Amendment Request" in
the status report and stop. Agent A is the only writer for that crate.

**Plan ordering is strict.** L1 lands first; subsequent rows depend on
the foundation L1–L4 establishes. Do not chain rows without explicit
"ship it" approval from the orchestrator.

## Architecture

```
Platform → Classifier → Lifecycle → Governor → UI
            (annotate)    (track)    (decide)   (render)
```

One tick per second by default. UI renders at 10 Hz with cached data.

`Platform` is an enum, not a trait — two backends in v1 (Linux,
Jetson). Refactor to `Box<dyn PlatformMetrics>` at 4+ backends, not
before.

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
- User-visible strings come from `ux_contract::{status,empty,confirm,
  errors,alerts}::*`. Hardcoded literals in `src/ui/` are caught by
  `tests/copy_strings_via_contract.rs` (added in L1).

### `unwrap()` and `expect()` rules

- No `unwrap()` outside tests.
- No `expect()` outside tests **except for documented invariants
  equivalent to "the binary is malformed (or its baseline runtime
  environment is broken beyond recovery) if this fails"**. Each such
  call **must** be preceded by an `// ok: expect — <one-line reason>`
  comment so reviewers and auditors can skip them, and
  `rg 'expect\(' src/` outside `#[cfg(test)]` must show only annotated
  lines. Accepted patterns:
  1. **Mutex-poison recovery** on a writer whose corruption is worse
     than a crash (e.g. the audit writer in `governor/audit.rs`, the
     lifecycle log store).
  2. **`Regex::new` inside `OnceLock`-initialised statics** where the
     pattern is a compile-time constant.
  3. **`reqwest::Client::builder().build()` in a sampler constructor**
     where the only failure mode is "the system's TLS / DNS resolver
     stack is broken" — at that point we cannot run anyway.

  Any new pattern beyond these requires updating this list (and
  reviewer signoff) — a one-off `// ok: expect` comment is not a
  licence to invent a new exemption.

## Safety rules (never violate)

1. Governor never kills allowlisted processes via automated policy
2. SIGTERM before SIGKILL, always, with a configurable grace period
3. Rate limit: max 3 automated kills per 60-second window
4. Every kill logged with reason, PID, name, model, timestamp to a
   persistent JSONL audit trail

## Historical note: dry-run mode removed in Sprint 1 lead (d8d7897)

Earlier versions had a dry-run / enforce policy mode in the governor.
Removed in favor of the `kill_confirm` card pattern (CAR-17 in
ux_contract v0.3.8) — the card overlay IS the kill safety surface
now. No more dry-run. If you see references to dry-run in commits or
comments older than `d8d7897`, treat them as historical context, not
current behavior.

## Historical note: Grafana integration removed in Sprint 5

The `g` keybinding, `[dashboard]` config section, WP5 TCP preflight
probe (`src/dashboard_preflight.rs`), and the `webbrowser` Cargo
dependency were all hard-deleted from v1.0 in Sprint 5. Rationale: the
integration was broken in practice and the v2 web companion (separate
repo) handles the dashboard story. The contract symbols
(`ux_contract::Action::OpenGrafana`, `ux_contract::status::
GRAFANA_UNREACHABLE`, `DASHBOARD_OPENED`, `DASHBOARD_FAILED`) remain
in the contract crate as orphans pending Agent A cleanup — the
dispatch arm for `Action::OpenGrafana` is a documented no-op in
`src/ui/mod.rs`. If you see code or commits referencing `handle_open
_dashboard` or `dashboard_preflight::probe`, treat them as historical
context.

## Environment

- **Dev + test host (primary):** bare Ubuntu 22.04.5 LTS on Gigabyte
  B560M H V2, kernel 6.8, x86_64. NOT WSL.
- **GPU:** NVIDIA GeForce RTX 3060 12GB, driver 595.58.03, CUDA 13.2.
  NVML is fully functional (`libnvidia-ml.so.595.58.03` installed).
- **ROS:** ROS Humble at `/opt/ros/humble`, RMW=Cyclone DDS
  (`rmw_cyclonedds_cpp`). Multiple workspaces sourced into shell
  (palb_ws, thesis_ws, yo_ws, turtlebot3_ws).
- **Production target (deployment, separate device):** Jetson AGX
  Orin via SSH. Hardware-touching features verified here before
  release.
- **NVML works on primary dev host**, but code must still handle
  `Option<GpuSnapshot>` gracefully — NVML can fail on environments
  WITHOUT NVIDIA hardware (CPU-only systems, certain containers,
  some WSL configurations). The graceful-degradation requirement
  is general, not WSL-specific.

## Known limitations

- **B-EMPIRICAL surfaced 2026-05-22:** RunStore records show
  `peak_vram_mb=0` on this NVML-working host. Root cause not yet
  determined. May be NVML permission gating, NVML init failure,
  per-PID VRAM accounting bug, or runtime.rs:1322 string-parsing
  issue. Tracked for next investigation cycle.
- **B-EMPIRICAL-4 surfaced 2026-05-22:** rclpy Python ROS2 nodes
  invisible to classifier on default Humble + Cyclone DDS. Three
  compounding causes — recommended fix in Inspector #9 report.
  v1.0.3 hotfix candidate.
- **Automated governor disabled by default (v1.0.1).** The
  `policy.default_ai_action` for AI workloads is `Allow`, not `Kill`.
  Inspector #1 caught a phantom-kill audit-trail bug in v1.0.0 where
  the governor logged automated kills without sending a signal; the
  v1.0.1 fix is to leave the default at Allow and require an explicit
  opt-in (`default_ai_action = "Kill"` in `edge_monitor.toml`) to
  enable automated kills. Manual kills via the `k` keybinding /
  `kill_confirm` card still work regardless. Surfaced in the `?` help
  overlay. Re-enabling automated kills also requires wiring
  `send_sigterm` into the executor — until that lands, opting in
  produces audit lines without real kills.
- **Ollama tokens/sec only available via `edge_monitor exec`**, not via
  passive monitoring. Ollama embeds tokens/sec in the per-request JSON
  response with no exposed Prometheus endpoint or log file — the only
  capture path is the exec wrapper's stdout parser (Tier 1.2d). A user
  who starts `ollama run …` independently and then watches it with
  edge_monitor will see "running actively" on the workload row,
  forever. Documented in the `?` help overlay and confirmed by the B4
  Sprint-2 investigation.
- vLLM and llama.cpp expose Prometheus and ARE scraped passively;
  tokens/sec for those flows through `LiveTelemetry` to the workloads
  panel within a tick or two of first sample.
- **(RESOLVED in Sprint 7 Item 3.)** ~~Workload start-time column
  reads "first observed", not OS spawn time~~ — the platform layer
  now reads `/proc/<pid>/stat` field 22 (`starttime`) plus
  `/proc/stat`'s `btime` to compute the real OS spawn timestamp, and
  the lifecycle tracker prefers that value when populated. The
  `first_observed_at` stamp survives as a fallback for processes
  whose `/proc` read fails (alien `/proc`, fakeproc, permission
  denied).
- **Grafana integration removed** (Sprint 5). The `g` keybinding is
  unbound and the v2 web companion (separate repo) handles the
  dashboard story. See "Historical note: Grafana integration removed
  in Sprint 5" above for the contract-orphan situation.
- **Windows binary on indefinite halt.** The sibling Windows repo (W1–
  W49 plan) is on hold pending operator-team scoping. Linux is the
  reference implementation; Windows parity catches up post-v1.0.
- **Web UI binds `0.0.0.0:7070` by default with NO AUTH.** The
  dashboard is reachable from any host on the same LAN; the v1.0
  design assumes a trusted-LAN posture (workstation / lab / robot
  dev fleet). On untrusted networks, pass `--bind 127.0.0.1` to
  restrict to localhost. A future release will add auth so the
  wider bind is safe by default. Documented in README "Web UI
  security" and surfaced as a startup `tracing::warn!` when the
  bind isn't a loopback address.

## When the user pushes for shortcuts

The user's system prompt asks for mentor-style pushback, not
convenience. If asked to:

- Skip tests → refuse, write tests first
- Merge or skip a row in the L1–L26 plan → refuse, cite the
  one-row-one-PR rule in the orchestrator instructions
- Edit `~/ux_contract` from this repo → refuse, file a Contract
  Amendment Request instead (Agent A owns it)
- Change UX behavior without amending v0.3 → refuse, cite the locked
  contract; the user can ask for an amendment if they want a change
- Stub hardware calls "for later" → refuse, handle errors now

## Session rhythm

- Start of session: re-read this file + the relevant clause(s) of
  UX_CONTRACT.md and the relevant L-row in DESIGN_HANDOFF.md.
- End of each non-trivial change: run
  `cargo test --workspace && cargo clippy --all-targets -- -D warnings`.
- Before opening a PR: confirm the row's "Test" column is satisfied,
  the binary still launches, and the diff stays inside the row's
  declared "Files to change" set.
