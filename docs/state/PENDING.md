# PENDING — things waiting on the human

*When you (the agent) hit a HARD STOP, write it here LOUDLY and stop. The human reads this at milestone check-ins. Clear an item when it's resolved (move the resolution to JOURNAL.md).*

*Format:*
```
## [STOP #N] <title> — <date>
**What I was doing:** ...
**Why I stopped:** (which HARD STOP rule)
**What I need from you:** (a decision / a CAR / a governor review / driver reload / etc.)
**My recommendation (if any):** ...
**What's safe to do meanwhile:** (other work I can proceed with, or "nothing — blocked")
```

---

## [FINDING] "TUI essentials-only" is ALREADY DONE as originally scoped — 2026-07-15

**What I was asked to do:** Phase 1's plan called for an investigator-pass on the "TUI essentials-only rework" (the last unstarted Phase-5 item per BOARD.md), then propose a design.

**What I found:** BOARD_AUDIT §3 (the source-of-truth ratified scope for the phrase) enumerates the "TUI-essentials rework" as EXACTLY four defects, all of which shipped in DISPATCH 107:

| BOARD_AUDIT §3 item | D107 FIX | Verifiable at |
| --- | --- | --- |
| Duplicate "AI Workloads" panel (unconditional at 5+ workloads) | FIX 1 | `src/ui/panels/mod.rs:249` — `render_workloads_two_col` fn removed, comment explains the change |
| No column headers on AI Workloads rows | FIX 2 | `src/ui/panels/workloads.rs:98,538` — new `column_header_line()` fn + call site |
| `sha256-…` digest leaking into workload NAME field | FIX 3 | `src/telemetry/samplers/ollama_api.rs` + `src/runtime.rs` — hint prefers friendly name, runtime promotes onto AnnotatedProcess.model_name |
| Vitals no aligned column grid / stranded RAM | FIX 4 | `src/ui/panels/vitals.rs` — LABEL_WIDTH=12 grid across every row |

**BOARD.md is stale on this point.** It says the phrase is "unstarted" but the phrase-as-defined shipped 2 dispatches ago. The BOARD update is a small doc landing I'll take as part of Phase 1 (not a HARD STOP).

**No design proposal needed for TUI-essentials-as-defined.** The phrase's originally-ratified scope is closed. Writing a proposal would be scope-invention — inspector's HARD STOP #3 discipline says "if no doc settles it, propose OPTIONS not decide" — but here the doc DOES settle it (BOARD_AUDIT §3), and it says done.

**If you WANT more TUI work — the candidate follow-ons that AREN'T shipped:**
These would each need their own scope decision (each is HARD STOP #3 if you want me to build any of them — I'd write a proposal per item). Enumerated for your reference; NOT proposing to build without ratification:

- **Hardware identity (`HostInfo`)** — BOARD_AUDIT §2.1 marks this as NEW / v1.4.x. Show GPU name (NVML `nvmlDeviceGetName`), CPU name (`/proc/cpuinfo`, no shellout), RAM identity. Open question: RAM = capacity label (free, procfs) vs DIMM part/speed (needs root `dmidecode` — footgun on an unprivileged tool). Open question: TUI-only vs wire to web.
- **AlertState raise/ack events into RuntimeState** — BOARD_AUDIT §3 surfacing gap V7. Signal exists internally; not accumulated onto the wire/UI.
- **Classifier consistency** — BOARD_AUDIT §2.2: "same binary (`claude`) lands in both Agent and Unknown; `bash` shows as a workload. Partial." Not TUI-cosmetic; classifier-logic scope.
- **Top Processes card on web** — BOARD_AUDIT §2.3 / §2.6: exists on TUI, missing from web. Web-parity gap.
- **Activity content parity (TUI vs web)** — BOARD_AUDIT §3 "Tester gate to confirm" — needs a diff pass to enumerate.

None of these are "TUI-essentials-only" per the ratified phrase. All are follow-on scope. Your call which (if any) to open.

**Autonomous action I took:** none for this item beyond writing this finding. BOARD update lands in the next commit. No landing 1.x needed.

---

## [STOP #3 — RESOLVED 2026-07-15] GPU temp/power tile — design ratified + SHIPPED

Operator confirmed inspector lean **1c / 2a / 3a**: VitalsPanel + KioskView
(skip Strip); one combined kiosk tile `62°C · 45W`; MAX temp / SUM watts
across devices. Backend + wire honesty landed in commit `814c1b3` (landing 3).
Web consumers landed in `e4772d3` (landing 4). Post-hoc design record at
[`docs/GPU_TILE_DESIGN.md`](../GPU_TILE_DESIGN.md). Resolution recorded in
JOURNAL.md.

---

## [STOP #3] GPU temp/power tile — design ratification needed — 2026-07-15

**What I was doing:** Landing 2 of this run — the BOARD-flagged "GPU temp/power tile (read + Prometheus exist, not surfaced), low-risk, buildable" item.

**Why I stopped:** No design doc exists for this feature. Investigation surfaced real design choices with materially different tradeoffs (placement scope, kiosk tile shape, aggregation). HARD STOP #3 fires — I propose, you decide.

**Signal availability — confirmed live:**
- Temp: NVML `device.temperature(TemperatureSensor::Gpu)` → `GpuDeviceMetrics.temp_c: Option<f32>` (degrees C) at [`src/platform/gpu_nvidia.rs:224-227`](../../src/platform/gpu_nvidia.rs#L224-L227).
- Power: NVML `device.power_usage()` (milliwatts) → `GpuDeviceMetrics.power_watts: Option<f32>` (watts) at [`src/platform/gpu_nvidia.rs:220-223`](../../src/platform/gpu_nvidia.rs#L220-L223).
- Prometheus surface exists: `edge_monitor_gpu_watts{pid=...}` and `edge_monitor_gpu_temp_celsius` at [`src/telemetry/exporter.rs:191-207`](../../src/telemetry/exporter.rs#L191-L207).
- **NOT on the TUI** ([`src/ui/panels/vitals.rs`](../../src/ui/panels/vitals.rs) reads `snap.gpu` for VRAM gauge only).
- **NOT on the web wire** — [`WireGpu`](../../src/web/wire.rs#L466-L472) has only `vram_pct` / `vram_used_mb` / `vram_total_mb` / `device_count`.

**Wire-type gap analysis (HARD STOP #2 test):** `WireGpu` is defined ENTIRELY in `src/web/wire.rs`, NOT in `../ux_contract`. Adding `temp_c: Option<f32>` + `power_w: Option<f32>` fields is a pure consumer-side additive change — **NO CAR needed** (HARD STOP #2 does NOT fire). Web `types.ts:145` mirror updates in lockstep.

**Design questions — needing your call:**

1. **Placement scope (which surfaces):**
   - **(a)** VitalsPanel + VitalsStrip + KioskView — everywhere. Most consistent, most work.
   - **(b)** VitalsPanel only (dashboard) — minimum, where the operator sits.
   - **(c) *Inspector lean:*** VitalsPanel + KioskView. Kiosk wall-monitor deserves it; VitalsStrip stays tight per D103's "chronology-first" intent.

2. **Kiosk tile shape (if included):**
   - **(a) *Inspector lean:*** One "GPU" tile showing `62°C · 45W` — one tile, two numbers, same signal source belong together.
   - **(b)** Two separate tiles "GPU TEMP" and "GPU POWER" — more granular, uses more space.
   - **(c)** Extend the existing "THERMAL" tile — mixes system/GPU thermals, blurs the signal boundary.

3. **Aggregation across devices:**
   - **(a) *Inspector lean:*** Max temp / sum watts across all `GpuDeviceMetrics` devices. Honest for 99% single-GPU hosts; sensible for multi-GPU.
   - **(b)** Primary device only — loses info on multi-GPU.
   - **(c)** Per-device rendering — more info, more UI space.

4. **Unmeasured handling — no choice, VRAM honesty rule applies:** NVML returns `None` for temp/power when Unsupported. Render as "—" with `data-testid-unmeasured="true"`, NEVER "0°C" or "0W". Same D95/D102 pattern that governs VRAM.

**My recommendation (all three "*Inspector lean*" defaults):**
- Scope: VitalsPanel (TUI + web dashboard) + KioskView. Skip VitalsStrip.
- Kiosk shape: one combined "GPU" tile — `62°C · 45W`. Grows kiosk from 3 to 4 big tiles.
- Aggregation: max temp / sum watts across devices.
- Unmeasured: "—" everywhere, honest.

**Build sequence if ratified (5 landings, ~2 hours):**
1. Wire additions to `WireGpu` — `temp_c: Option<f32>` + `power_w: Option<f32>`. Mirror `web/src/lib/types.ts`. Serialization site at `wire.rs:863`. Rust test pinning Some→field-present / None→field-absent (VRAM honesty on the wire).
2. TUI 6th row in `vitals.rs` — `GPU         62°C · 45W` on the 12-char label grid; unmeasured branch shows `—`.
3. Web `VitalsPanel.svelte` — extend GPU section with temp + watts + unmeasured branch.
4. Web `KioskView.svelte` — 4th tile with combined display + `data-testid-unmeasured` + D98 gate extension.
5. D98 matrix cells that assert kiosk tile count update from 3 to 4. New `F8_gpu_unmeasured.json` fixture pins the honesty discriminator at the wire boundary.

**What I need from you:** ratify (or redirect) the 3 design questions. A one-line "1c / 2a / 3a" (my lean) or your alternative gets me building landing 3.

**What's safe to do meanwhile:** the loop's other autonomously-completable work is thin — TUI essentials-only ALSO needs HARD STOP #3, and everything else in BOARD is human-blocked. If you don't want to ratify right now, I hit the EXIT condition — write a completion summary here and wait. Ratify at your leisure and I resume.

---

### Reference — the HARD STOP rules (from CLAUDE.md)
1. Governor / kill / actuation path touched — surface, never auto-proceed
2. A contract change (`../ux_contract`, new wire type, new endpoint) is needed — write a CAR, stop
3. An unratified design/UX decision (materially different approaches, no doc settles it) — propose options, don't decide
4. A destructive/irreversible action permissions didn't catch
5. About to arm the killer / enable auto_actuate / make a kill fire — never, surface
