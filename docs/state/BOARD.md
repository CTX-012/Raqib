# BOARD — current state

*The agent reads this at session start. Update it after every landing. Keep it CURRENT, not historical (history goes in JOURNAL.md).*

## HEAD
- Branch: `l14-top-processes-sort`
- Last tag baseline: `v1.3.2-46-gf91742a` + local landings through the top-processes 3-panel commit (unpushed)
- Rust tests: 1267 / 0 fail (workspace; +8 classifier tests: 7 Gazebo detect/precision + 2 rviz2 tripwire — one Gazebo test reused an assert)
- Binary name: `raqib` (renamed 2026-07-30). Library crate `edge_monitor` (unchanged — internal identifier only).
- Browser render-gate: 269 / 0 (`npm --prefix web run test:browser`, unchanged — onboarding is CLI + config path, no web-render surface)
- SCHEMA firewall: 5 / 5
- ux_contract: v0.3.21 (sibling repo `../ux_contract` — DO NOT EDIT)

## What's shipped (don't rebuild)
- Phases 1-3: complete (honest sampling, real samplers, vitals+governor)
- Auto-kill: complete, DORMANT + hardware-verified inert. Detects VRAM+RAM+thermal, decides, acts (SIGTERM→SIGKILL). auto_actuate defaults FALSE. Arming is console-only. **NEVER touch this path autonomously — HARD STOP #1.**
- History: complete both surfaces (web view + trajectory charts + post-mortem curves; TUI event timeline). Capture is dormant/write-only, read only at endpoint + UI layer.
- Display modes: 5 live (dashboard/history/kiosk/timeline/focus), URL-param switching, 221-assertion gate.
- Cleanup: TUI cluster + LOW sweep done.

## Open items (safe to work on autonomously unless flagged)
- ~~TUI essentials-only~~ — ✅ SHIPPED. Investigator finding 2026-07-15 (see PENDING.md): the phrase-as-ratified in BOARD_AUDIT §3 was exactly 4 defects, all closed by DISPATCH 107 (duplicate panel / column headers / sha256 leak / vitals grid). Any additional TUI work is FOLLOW-ON scope — enumerated as candidates in PENDING.md, each needing its own scope decision.
- ~~GPU temp/power tile~~ — ✅ SHIPPED (D109 landings 3 + 4). VitalsPanel + KioskView both surface the aggregate; F6 fixture updated; gate at 223/0. Pushed 2026-07-15.
- **(no open items — everything remaining is human-blocked or hardware-blocked)**

## Blocked — needs human (do NOT attempt)
- **Versioning decision** — two Phase-5 arcs under `[Unreleased]`; whether to tag v2.0.0. HUMAN DECISION.
- **observer→supervisor decision** — the strategic call; gates auto-kill default-on + external-kill codes. HUMAN DECISION.
- step-9 auto-kill tiebreaker — needs a `LiveTelemetry::last_active_at` field AND touches the governor — HARD STOP #1.
- VRAM verification / measured-path — needs the GPU driver reloaded (human, 5-min). Until then, unmeasured VRAM is the common case (see the VRAM honesty rule).
- ITEM 2: dead const `KILL_ARM_WINDOW_SECS` in `../ux_contract` — trivial removal but it's the contract crate — CAR (HARD STOP #2), rides the next contract bump.

## Pending human action (check PENDING.md)
- (none right now — D109 landings pushed 2026-07-15, operator confirmed. `git fetch` in this shell cannot verify remote-tracking-ref sync due to no cached creds — the audit sweep in Phase 2 flags this ceiling explicitly.)

## Config note
- The binary reads config from `./edge_monitor.toml` (CWD) or `--config <path>`, NOT `~/.config/`. The repo-root `edge_monitor.toml` has `[web] allow_no_auth = true` (open API, no token for curl/smoke). `--bind 127.0.0.1` for local-only.
