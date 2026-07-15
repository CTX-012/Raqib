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

## [STOP #3 — RESOLVED 2026-07-15] GPU temp/power tile — design ratified

Operator confirmed inspector lean **1c / 2a / 3a**: VitalsPanel + KioskView
(skip Strip); one combined kiosk tile `62°C · 45W`; MAX temp / SUM watts
across devices. Backend + wire honesty landed in commit `814c1b3` (landing 3).
Landing 4 (web consumers) in progress. Resolution recorded in JOURNAL.md.

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
