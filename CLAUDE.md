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

## Environment caveats

- Primary dev env: WSL Ubuntu. Limited — no real GPU, no thermal, fake
  `/sys`. Don't trust WSL for GPU/thermal feature verification.
- Real test target: Jetson AGX Orin via SSH. Any feature touching
  hardware must be tested on Orin before merge.
- NVML may fail to initialize on WSL. Code must handle
  `Option<GpuSnapshot>` everywhere. Never panic on missing NVML.

## Known limitations

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
