# edge_monitor — Board Audit

**Plan vs Shipped vs Left, board by board.**

- **Date:** 2026-06-05
- **State (verified via git, prior session):** edge_monitor `v1.3.1` (HEAD `f3e7607`, 970 tests) · ux_contract `v0.3.16`
- **Repos:** `~/edge_monitor-l14` (branch `l14-top-processes-sort`), `~/ux_contract`
- **In-repo canon:** `docs/ROADMAP.md`, `docs/PHASE4_DESIGN.md`
- **In flight this session:** DISPATCH 57 — v1.3.2 (per-workload rules + web cache-header fix). Web System-zero bug **cleared** (stale browser client, not a backend regression).
- **TUI defects (observed Jun-5 during v1.3.2 gate):** duplicate AI-Workloads panel (unconditional, not overflow), no column headers, `sha256` name leak, Vitals no-grid/stranded-RAM — all **folded into Phase 5** (TUI-essentials rework), NOT v1.3.2 regressions.
- **Verification:** DISPATCH 58 (Inspector, read-only) **closed all 7 [VERIFY] items** — 3 confirmed as audited, 3 corrected to more-nuanced, 1 backlog gap surfaced, **0 new bugs**, `serve_asset` untouched. Findings folded in below with file:line cites.

## 0. How to read this

This is a per-board ledger of what was *planned*, what is *shipped* (with evidence), and what is *left*. Sources, in priority order:

1. **`73675718` "Memory activation and data retrieval"** — the canonical edge_monitor orchestration chat (DISPATCH 14/34/35, Phase 3 re-anchor, governor lock, §1–13 handoff, web-zero/DISPATCH 56).
2. **`28253d9f` "Merge all clean reports from 26/5"** — cross-project status digest.
3. **DISPATCH 58 Inspector report** — read-only source verification (file:line cites).
4. **Live screenshots (Jun 5)** — TUI, History overlay, web companion.

Items previously tagged **[VERIFY]** were inferred from screenshots/chat; **DISPATCH 58 resolved all of them** and the confirmed/corrected results are inline below (look for **[58: …]**).

## 1. Roadmap backbone — the 5 phases

| Phase | Version | Planned scope | Status |
|---|---|---|---|
| 1 | v1.0.1 | Bug closure — stop the "running actively" lie | ✅ shipped (8/12 lying bugs) |
| 2 | v1.1.x | Per-category samplers + activity-field separation (+ classifier, `ActivityState` → ux_contract v0.3.12) | ✅ shipped |
| 3 | v1.2.0 | Vitals + Governor-finish + **cancel-countdown** + config-file settings (**Adopt-C**: config pulled forward) | ✅ shipped (governor observe-only) |
| 4 | v1.3.x | **Web SPA + settings UI + auth** (reads TOML) — **+ auto-kill management (new scope)** | ⏳ in progress (v1.3.1 → v1.3.2) |
| 5 | v2.0.0 | **History data-model rebuild + 5 web display modes + TUI essentials-only** | ⏳ in progress — History subsystem (D88-D97) + 5 web display modes (D99-D106) ✅ shipped under `[Unreleased]`; TUI essentials-only pass NOT started |

**Drift notes (reconciled):**
- Phase 3 was re-anchored (DISPATCH 35) to **2 subsystems: Vitals + Governor-finish**, config *deferred*. The June 3 digest then shows **Adopt-C pulling config forward into Phase 3** — which is why v1.3.2 can do per-workload config rules (the TOML layer already exists).
- **Version label drift:** Phase 5 = `v2.0.0` (digest) vs `v1.4.0` (May-21 handoff). Phase→feature mapping is stable; the version label is not pinned.
- **Governor was never unbuilt:** ~1,626 LoC across 6 files already shipped. Phase 3 finished its *observe/recommend surface*, not its authority.
- **Auto-kill is net-new Phase 4 scope** grafted on beyond "SPA + settings + auth" — the deliberate crossing of the observe-only line (held as a lock 9×). Owes its own safety pre-pass. **[58 reframe]** the thermal trigger's signal already exists (NVML temp → Prometheus); the work is *wiring it into the governor*, not acquiring it (see 2.1).

## 2. Board ledger

### 2.1 Vitals  *(top of TUI)*

**Planned:** base signals (load avg, cpus, RAM, VRAM, process count, workload count) + Phase 3 thermal zones read from `/sys/class/thermal` via `std::fs` (no shellouts) through the `HostVitals` wire type (ux_contract v0.3.13). Power consumption + INA3221 **deferred**.

**Shipped** — confirmed live (TUI): load avg, cpus 16, RAM, VRAM (NVML working), process count, workload count, `thermal: x86_pkg_temp / acpitz / acpitz`. The thermal expansion renders correctly.

**Left:**
- **GPU temp — [58: CONFIRMED read, surfaced Prometheus-only, NO governor hook].** NVML `device.temperature(Gpu)` at `gpu_nvidia.rs:224`; stored `GpuDeviceMetrics.temp_c`; folded per-PID into `TelemetryFrame.gpu_temp_c` (`source.rs:189`); exported as `edge_monitor_gpu_temp_celsius` (`exporter.rs:205`). **NOT** on TUI vitals (`vitals.rs` reads `/sys` thermal zones only), **NOT** on web wire, **NOT** in governor (`grep src/governor` thermal = 0). → Thermal auto-kill = wire the *existing* signal into the governor. Surfacing-gap **filed as backlog** (TUI/web tile).
- **Power consumption — [58: CONFIRMED read, Prometheus-only; `power_rails` is a stub].** NVML `device.power_usage()` at `gpu_nvidia.rs:220` → `edge_monitor_gpu_watts` (`exporter.rs:191`). But `HostVitals.power_rails` (v0.3.16) is **never populated** — all 6 sites emit `Vec::new()` (`host_vitals.rs:92`…); no INA3221 sysfs collection exists. → Surfacing-gap **filed as backlog** (TUI/web tile); INA3221 stays deferred.
- **INA3221** — deferred indefinitely (no Jetson hardware). Confirmed unwired (`power_rails` stub). Correctly parked (v1.3.3).
- **Web thermal — [58: CONFIRMED renders, no gap].** `wire.rs:683` populates `thermal_zones` with pre-classified severity; `VitalsPanel.svelte:42-105` renders top-3 color-coded with "N of M shown". (Terminology: it's `VitalsPanel.svelte`, not a "System card".)
- **No column grid [OBSERVED Jun-5]** — Vitals values are whitespace-positioned, not an aligned grid. On a **No-GPU host** the VRAM line is absent and RAM floats far-right with a large dead gap — reads as broken. → **Phase 5 (TUI-essentials rework).**
- **Hardware identity — NEW (v1.4.x):** show GPU name (NVML `nvmlDeviceGetName`), CPU name (`/proc/cpuinfo`, no shellout), RAM identity. Static data → read-once `HostInfo`/`SystemIdentity` at `Runtime::new` (resolve-once pattern), **not** the per-tick `HostVitals` wire. **Open decision:** RAM = capacity label (free, procfs) vs DIMM part/speed (needs root `dmidecode` — footgun on an unprivileged tool). **Open decision:** TUI-only vs wire to web.

### 2.2 AI Workloads  *(the biggest board)*

**Planned:** category grouping (LLM / Agent / Unknown), per-class samplers (ollama/vLLM/YOLO/ROS2/Claude Code), `ActivityState` separated from `WorkloadStatus` (→ ux_contract v0.3.12), classifier, per-workload row display.

**Shipped:** category grouping, per-class samplers (`sample_with_context` two-slice contract, `sample_timeout()`), classifier broadened, `ActivityState` field separated, zombie/ghost-row filter. *The engine largely landed in Phase 1–2.*

**Left:**
- **Duplicate-panel glitch [OBSERVED Jun-5 — worse than audited]** — TUI renders **two boxes both titled "AI Workloads"**, LLM/Agent split mirrored across them. Confirmed live at **5 workloads** — i.e. it fires *unconditionally*, NOT just on height overflow as previously assumed. Routed to "Phase 3 cleanup," never fixed. → **Phase 5 (TUI-essentials rework).**
- **No column headers [OBSERVED Jun-5]** — rows read `ollama 2h ago cpu 0% rss 60M`; Name / last-seen / activity / metric / RSS unlabeled; the inline `cpu`/`rss` tokens read as noise. Never shipped. → **Phase 5 (TUI-essentials rework).** *(Closes the "is the activity real?" confusion — activity itself is confirmed real per V5.)*
- **Activity-signal rendering — [58: CORRECTED — it's REAL].** The row token is a real `ActivityState` enum (4 strings: active/idle/loading/—) at `workloads.rs:357`, sourced from samplers via `Dispatcher::activity_for(pid)`. The CPU-heuristic is **internal to the Embeddings sampler only** (`embeddings_cpu.rs`); other categories use real signals (Ollama `/api/ps`, ROS2 echo-once, agent PPID). The panel never renders raw `cpu_pct`. → The activity is honest; only the **column header** is missing.
- **BUG-P5-2 sub-Hz ROS2 — [58: SHIPPED v1.1.5, CLOSED].** ITEM D: replaced sub-Hz-blind `ros2 topic hz` with `ros2 topic echo --once` + 30s staleness (`ros2_shellout.rs:9-23`, `CHANGELOG [1.1.5]`); v1.1.6 dropped `--timeout` for Humble compat. Not open. *(Removed from backlog.)*
- **Exit-capture "unknown"** — exit codes show "unknown" (see History); the `… exited with unknown — press Enter for post-mortem` alerts were deferred with "routing TBD." **Straddles the Post-mortem + Activity boards — same root gap.**
- **Classifier consistency** — same binary (`claude`) lands in both Agent and Unknown; `bash` shows as a workload. Partial.
- **Workload-name digest leak [observed Jun-5]** — the right-panel `ollama` row shows `sha256-eb2c71…` in the **name** field (a raw image digest leaking into the display name instead of the model/process name). New defect, observed live during the v1.3.2 gate.

> **Phase 5 reframing:** "TUI essentials-only" intent means heavy display migrates to web. The duplicate-panel and column-header items may be *intentionally* deprioritized on the TUI if display is moving to the web.

### 2.3 Top Processes  *(by RAM)*

**Planned:** Phase 3 deliverable; the active branch `l14-top-processes-sort` names **sorting** as the feature.

**Shipped:** RAM-sorted top-5 renders (TUI): chrome / code / chrome / chrome / chrome with CPU%.

**Left:**
- **Sort toggle — [58: SHIPPED, and bigger than audited].** Implemented as a **3-state cycle RAM → CPU → VRAM** (not a binary RAM↔CPU swap): `TopProcessesSort` enum (`top_processes.rs:62`), `t` keybind (`input.rs:54`), `App.top_sort` state (`app.rs:186`), routed render + tests. **Done.** *(Audit phrasing corrected.)*
- **Web parity gap** — web companion has no Top Processes card at all. TUI-only. **Still open.**
- **Configurable row count** — fixed 5 (minor; likely never planned).

### 2.4 Activity  *(feed, bottom of TUI)*

**Planned:** activity feed of workload lifecycle events + alerts. `AlertState` lifted to Runtime so alerts fire headless; `ACTIVITY_FEED_*` constants in ux_contract v0.3.10.

**Shipped — [58: 3 sources wired, render correct]:** the feed reads `state.completed` (AI-classified exits, `runtime.rs:684`), `state.audit` (governor kill/cancel/abort), and `state.regressions` (Tier 1.3 events); merged time-descending, capped at 5 (`activity.rs:94,144`). Empty-looking feeds = **no events have fired** (no exits, default-Allow governor + no manual kills, <3 baseline samples), **not** a render gap.

**Left:**
- **AlertState raise/ack events — [58: documented BACKLOG, now filed].** `observe()` / `ack_all()` produce events but they're **not accumulated into `RuntimeState`** (`activity.rs:21-25`). Accumulating them closes the gap. **Filed as backlog.**
- **Exit-driven entries** depend on exit-capture (currently "unknown") — tied to the shared spine (2.2 / 2.5 / 2.7).
- **Web Activity parity** — web has an Activity card; content parity to be confirmed by the v1.3.2 web-render Tester gate.

### 2.5 History  *(the `h` overlay)*

**Planned:** history view exists; Phase 5 = **history data-model rebuild**; operator note: *"history card, worst, need real fix."*

**Shipped:** the `h` overlay works (History overlay: "19 runs · columns: # When Dur Exit"); pre-v1.0 governor dry-run rows retained for archaeology ("dry-run mode — record retained for archeology").

**Left:**
- **Exit = "unknown" on every run** — the core defect; exit codes aren't captured. The governor dry-run rows are the only non-"unknown" entries.
- **Phase 5 data-model rebuild** — not started.
- **The "real fix" redesign** — operator flagged the history card as the worst board.
- Duration/exit semantics depend on the rebuilt data model.

### 2.6 Web companion  *(localhost:7070 — the live frontier)*

**Planned:** Phase 4 = **web SPA + settings UI + auth** (reads TOML from Phase 3). **New scope:** auto-kill management — web toggles it on, sets temp/VRAM/RAM thresholds, system auto-SIGTERMs to prevent OOM. Phase 5 = **5 web display modes + TUI essentials-only**.

**Shipped:** read-only dashboard (localhost:7070): System / AI Workloads / Activity cards; thermal renders (2.1); footer "web companion (read-only) · use the TUI for control"; live tick. v1.3.0 ✅, v1.3.1 ✅ validated, v1.3.2 in progress.

**Left:**
- **Settings UI** — not built.
- **Auth** — currently unauthenticated (read-only today; becomes a real security gap the moment mutation/auto-kill lands).
- **Auto-kill actuation + safety pre-pass** — *the frontier.* Owes: opt-in/off-by-default (v1.0.1 phantom-kill scar), where SIGTERM executes (local tick-loop vs network handler — network-in-kill-path is the open safety question), kill tiebreaker (lowest-PID 50% → least-recent-activity; the activity sampler it needs **exists**, 2.2), sustain gate, web-mutation auth, and deliberate teardown of the 3 observe-only firewalls (type/wiring/config). **[58]** thermal trigger only needs governor-side wiring of the existing NVML signal.
- **5 web display modes** — ✅ **SHIPPED** (D99-D106, under `[Unreleased]`). Five URL-switchable modes (Dashboard / History / Kiosk / Timeline / Focus); design + build history in [`PHASE5_DISPLAY_MODES_DESIGN.md`](../PHASE5_DISPLAY_MODES_DESIGN.md). D98 browser render gate expanded to 221 assertions (5×7 mode×fixture matrix + per-mode probes). Zero new endpoints, zero contract touches. Step 9 (`/api/live/trajectory/{pid}`) explicitly deferred to v-next / v1.4.x candidate.
- **Top Processes card** — missing from web (2.3).
- **Cache-header durable fix** — folding into v1.3.2 (the web-zero root: `serve_asset` sends no `Cache-Control`/`ETag`, so browsers never revalidate after a rebuild).

### 2.7 Post-mortem  *(Phase 5)*

**Planned:** Phase 5 = **post-mortem rebuild** — capture what happened when a workload dies; entry point is "press Enter for post-mortem" on exit.

**Shipped:** the exit-alert line exists (`… exited with unknown — press Enter for post-mortem`), but the post-mortem **view is not built** (routing TBD).

**Left:**
- **Everything** — the post-mortem view itself; depends on exit-capture (currently "unknown"); shares the rebuilt data model with History (2.5). Not started.

## 3. Cross-board backlog (consolidated)

**Phase 5 — TUI-essentials rework (observed Jun-5, folded into Phase 5 per operator):**
- Duplicate "AI Workloads" panel — fires *unconditionally* at 5 workloads, not just on overflow (2.2)
- No column headers on AI Workloads rows (2.2) — activity is confirmed real (V5); only the header is missing
- `sha256-…` digest leaking into the workload **name** field (2.2)
- Vitals has no aligned column grid; RAM floats / strands on No-GPU hosts (2.1)
- Rationale: Phase 5 is "TUI essentials-only" — display migrates to web; polishing TUI layout now would be partly thrown away. Fix as part of the rework.

**Surfacing gaps — filed from DISPATCH 58 (signal exists, not displayed):**
- NVML GPU temp → add TUI/web vitals tile (read + on Prometheus, absent from TUI/web) (2.1 / V1)
- NVML power → add TUI/web vitals tile (read + on Prometheus, absent from TUI/web) (2.1 / V2)
- `AlertState` raise/ack events → accumulate into `RuntimeState` so the feed shows them (2.4 / V7)

**Architectural (v1.4.x / v2.0.0):**
- Exit-capture "unknown" → History + Activity + Post-mortem all depend on it (2.4 / 2.5 / 2.7) — *highest-leverage spine*
- History data-model rebuild (2.5)
- Post-mortem view (2.7)
- ~~5 web display modes (2.6)~~ ✅ shipped D99-D106 (2026-07-08, under `[Unreleased]`)
- Hardware identity `HostInfo` (2.1)

**Web parity gaps:**
- Top Processes card missing on web (2.3)
- Activity content parity (2.4) — Tester gate to confirm

**Deferred — no hardware:**
- INA3221 / `power_rails` population (2.1, confirmed stub) + Jetson AGX Orin VRAM re-run + v1.3.3

**Safety (the auto-kill frontier — own pre-pass):**
- Opt-in/off-by-default, SIGTERM execution site, tiebreaker, sustain gate, web auth, firewall teardown, governor thermal-wiring of the existing NVML signal (2.1 / 2.6)

**DISPATCH 62 findings (routed 2026-06-08):**
- **62-E (MED, latent)** → `KillAction::AlreadyPending` rate-limit-drain fix → **PREREQUISITE in PHASE4_AUTOKILL_DESIGN (step 0)**, lands before/with actuation step 3
- **62-A (MED)** → residual "running actively" lie: `primary_metric()` returns `RUNNING_ACTIVELY` for ROS2/Embeddings/Unknown even when `ActivityState=Idle`; narrow tier hides the contradiction → **standalone fix dispatch now** (`workloads.rs:380-382`)
- **62-C (MED)** → sha256 runner-digest leak root-caused to `ollama_api.rs:356` → already in Phase 5 TUI cluster
- **v1.3.x LOW cleanup batch:** 62-B (CHANGELOG "twelve"→11), 62-D (dead const `KILL_ARM_WINDOW_SECS`), 62-G (`config_schema_firewall.rs` SCHEMA_PATHS fragility if schema splits files), 62-F (v1.3.2 tag — resolves when operator tags post-smoke-test)

**Closed by DISPATCH 58 (no longer backlog):**
- BUG-P5-2 sub-Hz ROS2 — shipped v1.1.5 (2.2)
- Top Processes sort — shipped, 3-state cycle (2.3)
- Web thermal render — confirmed renders (2.1)
- Activity-signal authenticity — confirmed real (2.2)

## 4. Verification — CLOSED by DISPATCH 58 (Inspector, read-only)

| # | Item | Verdict | Cite |
|---|---|---|---|
| V1 | GPU temp via NVML | **CONFIRMED read; Prometheus-only; NO governor hook** | `gpu_nvidia.rs:224`, `exporter.rs:205`, `src/governor` (none) |
| V2 | NVML power / INA3221 | **CONFIRMED read; Prometheus-only; `power_rails` stub; no INA3221 collection** | `gpu_nvidia.rs:220`, `exporter.rs:191`, `host_vitals.rs:92` (empty) |
| V3 | Web thermal render | **CONFIRMED renders** | `wire.rs:683`, `VitalsPanel.svelte:42-105` |
| V4 | BUG-P5-2 sub-Hz ROS2 | **CONFIRMED shipped v1.1.5** (echo --once + staleness); v1.1.6 Humble patch | `CHANGELOG [1.1.5]`, `ros2_shellout.rs:9-23` |
| V5 | Activity token | **CORRECTED — real `ActivityState`** (4 strings); CPU-heuristic internal to Embeddings only | `workloads.rs:357,234`, `embeddings_cpu.rs` |
| V6 | Top Processes sort | **CONFIRMED — 3-state cycle RAM→CPU→VRAM** (`t`), not binary | `input.rs:54`, `app.rs:186`, `top_processes.rs:62,211` |
| V7 | Activity feed sources | **CONFIRMED 3 wired** (completed/audit/regressions); AlertState raise/ack **BACKLOG** | `activity.rs:1-25,94,144` |

Tally: **confirmed as audited** V3 V4 V6 · **corrected (more nuanced)** V1 V2 V5 · **new bugs** 0 · `serve_asset` untouched, DISPATCH 57 write surface intact.

---

*Living document — drafted from `73675718` + `28253d9f` + DISPATCH 58 + Jun-5 screenshots. Promote into the repo (e.g. `docs/state/BOARD_AUDIT.md`), archive on update, operator owns, orchestrator drafts.*
