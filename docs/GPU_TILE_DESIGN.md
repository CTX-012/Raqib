# GPU temp/power tile — design (SHIPPED)

> **Status**: **SHIPPED — DISPATCH 109 (landings 3 + 4), 2026-07-15.**
> Retroactive design doc — captures the decisions that landed
> under the D99-style design-first pattern applied post-hoc.
> Baseline at close: `v1.3.2-40-ga22bcba`, 1184 rust tests, gate
> 223 / 0.
> Related: [`PHASE5_DISPLAY_MODES_DESIGN.md`](PHASE5_DISPLAY_MODES_DESIGN.md)
> (the kiosk tile grew from 3 to 4 as part of this).

---

## 0. What this closed

BOARD_AUDIT §2.1 flagged two "surfacing gaps" — the signals existed
end-to-end (NVML → per-process telemetry → Prometheus exporter) but
never reached the operator-facing surfaces (TUI Vitals panel, web
dashboard, web kiosk):

- **V1**: NVML GPU temperature (`device.temperature(TemperatureSensor::Gpu)`)
- **V2**: NVML GPU board power (`device.power_usage()`)

Both fields were already read at
[`src/platform/gpu_nvidia.rs:220-227`](../src/platform/gpu_nvidia.rs#L220-L227)
into `GpuDeviceMetrics.temp_c: Option<f32>` and `.power_watts:
Option<f32>`. Both surfaced as Prometheus gauges
(`edge_monitor_gpu_temp_celsius`,
`edge_monitor_gpu_watts{pid=...}`). Neither reached the human-
readable surfaces the operator actually watches.

## 1. Design decisions ratified (Inspector lean 1c / 2a / 3a)

Investigator surfaced three design questions to `PENDING.md` on
2026-07-15; operator ratified the inspector lean the same day.

### 1a. Placement scope → **1c**: VitalsPanel + KioskView

| Option | Surfaces | Chosen? |
| --- | --- | --- |
| 1a | Everywhere (VitalsPanel + VitalsStrip + KioskView) | ❌ |
| 1b | VitalsPanel only | ❌ |
| **1c** | **VitalsPanel + KioskView (skip Strip)** | **✅** |

Rationale (operator-ratified): dashboard's VitalsPanel is where
the operator sits (natural home for a new signal); kiosk's
wall-monitor use case deserves the big-tile treatment (BOARD's
"is anything on fire?" question is exactly what temp/power
answer). Timeline's `VitalsStrip` is deliberately chronology-
first per D103 intent — a GPU signal doesn't add much to a
compact event-log context.

### 1b. Kiosk tile shape → **2a**: one combined `62°C · 45W` tile

| Option | Shape | Chosen? |
| --- | --- | --- |
| **2a** | **One combined `GPU: 62°C · 45W` tile** | **✅** |
| 2b | Two separate tiles ("GPU TEMP" + "GPU POWER") | ❌ |
| 2c | Extend existing "THERMAL" tile | ❌ |

Rationale: temp and power come from the same signal source
(`gpu_nvidia.rs` NVML), belong together conceptually. One tile
grows the kiosk grid from 3 → 4 tiles (RAM · VRAM · **GPU** ·
Thermal); two separate tiles would have grown it to 5, awkward on
narrow displays. Extending "Thermal" would blur the system-thermal
vs GPU-thermal boundary.

### 1c. Aggregation across devices → **3a**: MAX temp / SUM watts

| Option | Aggregation | Chosen? |
| --- | --- | --- |
| **3a** | **MAX temp / SUM watts across all devices** | **✅** |
| 3b | Primary device only | ❌ |
| 3c | Per-device rendering | ❌ |

Rationale: 99% of hosts have 1 GPU; multi-GPU aggregate is honest
without cluttering the tile. MAX temp matches the TUI thermal
panel's "hottest zone drives the row" convention. SUM watts
answers the operator's total-board-draw question.

## 2. VRAM honesty rule (the load-bearing invariant)

Unmeasured VRAM must render as `—` NOT `0` — the discriminator
that lets a "no measurement" tick be visually distinct from a
"measured zero" tick. This rule now covers GPU temp and power
too. Extended in four places, all on the same skip-serializing-if
+ `data-testid-unmeasured` pattern D95/D102/D103 established:

- **Wire boundary** (`src/web/wire.rs`): both fields
  `Option<f32>` with `#[serde(skip_serializing_if =
  "Option::is_none")]`. Unmeasured → absent from JSON, NEVER
  JSON `null` or coerced `0`. Pinned by three tests:
  - `wire_gpu_temp_and_power_none_omit_fields_from_json`
  - `wire_gpu_temp_and_power_some_zero_serialize_as_zero_not_omitted`
  - `wire_gpu_temp_and_power_some_nonzero_serialize_normally`
- **TUI vitals row**
  ([`src/ui/panels/vitals.rs`](../src/ui/panels/vitals.rs)):
  `temp_c.map_or_else(|| "—".to_string(), |t| format!("{t:.0}°C"))`;
  power the same shape.
- **Web VitalsPanel inline line**
  ([`web/src/components/VitalsPanel.svelte`](../web/src/components/VitalsPanel.svelte)):
  per-half `#if temp_c !== undefined` branches; unmeasured span
  gets `data-testid-unmeasured="true"`.
- **Web KioskView big tile**
  ([`web/src/views/KioskView.svelte`](../web/src/views/KioskView.svelte)):
  same per-half discriminator; belt-and-braces D98 gate
  assertion FAILS LOUDLY if `0°C` or `0W` ever appears.

**Testability ceiling:** the *measured* path renders end-to-end
live (this session's dev host has the driver loaded and NVML
returns real values). The *unmeasured* path is pinned by tests
(the three wire tests above + the D98 gate's `data-testid-
unmeasured` assertions on F1/F2/F3 fixtures with `gpu: null`).
On a driver-unloaded host, the live rendering path additionally
verifies unmeasured — that's the state BOARD calls "the common
case on this host," but driver state changes between sessions.
The tests hold regardless.

## 3. Ship record (dispatches)

### Landing 3 — backend (D109/L3, commit `814c1b3`)

- `src/web/wire.rs` — `WireGpu` fields + builder aggregation +
  3 wire-honesty tests
- `src/ui/panels/vitals.rs` — layout 5 → 6 rows; GPU row at
  cols[3]; thermal shift cols[4] → cols[5]
- Tests: 1181 → 1184

### Landing 4 — web consumers (D109/L4, commit `e4772d3`)

- `web/src/lib/types.ts` — `WireGpu` interface mirror (both
  fields `temp_c?: number` / `power_w?: number`)
- `web/src/components/VitalsPanel.svelte` — inline GPU line
  beneath the VRAM bar
- `web/src/views/KioskView.svelte` — 4th tile; grid
  `md:grid-cols-3` → `sm:grid-cols-2 md:grid-cols-4`
- `tests/fixtures/render_adversarial/F6_kiosk_all_criticals.json`
  — gains `temp_c: 87.0, power_w: 175.0`
- `web/tests/browser_render_gate.mjs` — kiosk probe extended
  (tile count 3 → 4; new `expectGpuTempMeasured` /
  `expectGpuPowerMeasured` flags on F6; belt-and-braces
  coerced-zero guard)
- Gate: 221 → 223
- Bundle etag flipped `f3715c48...` → `be2dcacf...` (release
  binary needed `touch src/web/assets.rs` to force rust-embed
  invalidation — documented for future sessions)

## 4. What this dispatch deliberately did NOT do

- **NO contract touch.** `WireGpu` lives entirely in
  `src/web/wire.rs`; the additive `Option<f32>` fields are a
  consumer-side change. No `ux_contract` bump required.
- **NO governor touch.** The kill path is untouched — the GPU
  temp signal exists at the platform layer and Prometheus
  exporter today; wiring it into the governor (threshold-based
  auto-kill on overheat) is a separate scope decision that
  requires the observer→supervisor human decision (BOARD).
- **NO VitalsStrip growth.** Timeline mode intentionally stays
  chronology-first; adding a GPU column to the compact strip
  would clutter without adding to timeline's operator question
  ("what happened when?"). Follow-on scope if wanted.
- **NO per-device rendering.** The multi-GPU case (rare on the
  target hosts) aggregates via MAX/SUM. Per-device follows the
  same pattern D95 established for VRAM (the `device_count`
  field on `WireGpu` signals multi-device without cluttering
  the UI); a follow-on dispatch could add per-device drill-in.

## 5. Follow-on candidates (deliberately deferred)

If the operator wants more from this signal, these are the
natural extensions — each needs its own scope decision, none
attempted here:

- **Governor auto-kill on GPU thermal** (BOARD_AUDIT §2.1 "GPU
  temp — CONFIRMED read, surfaced Prometheus-only, NO governor
  hook"). Wiring the existing signal into the governor's
  threshold-breach path. Owes: observer→supervisor decision;
  the auto-actuate + tiebreaker discipline; explicit HARD STOP #1
  review — this is kill-path work.
- **INA3221 / `power_rails` population** (BOARD_AUDIT §2.1).
  Currently a stub. Deferred pending Jetson hardware.
- **Per-device rendering** — the drill-in path in kiosk (click
  → per-GPU breakdown). Kiosk is glance-only per D102, so this
  is more likely a `?mode=focus&gpu=N` extension. Not scoped.
- **VRAM % on kiosk severity aggregation** — currently VRAM %
  is one of the inputs to the kiosk's overall severity; GPU
  temp could also drive severity (85°C / 95°C amber/red per
  contract thresholds). Deliberately NOT wired in D109 —
  the kiosk overall-severity aggregation is a follow-on scope
  choice (which signals contribute? equal weight?).

## 6. Summary

| Field | Value |
| --- | --- |
| **Dispatches** | D109 landings 3 + 4 |
| **Design ratified** | 2026-07-15 (Inspector lean 1c/2a/3a) |
| **Rust wire additions** | `WireGpu.temp_c: Option<f32>` + `power_w: Option<f32>` |
| **TS mirror** | `WireGpu.temp_c?: number` + `power_w?: number` in `web/src/lib/types.ts` |
| **TUI change** | Vitals panel 5 → 6 rows; new GPU row |
| **Web changes** | VitalsPanel inline line + KioskView 4th tile |
| **Fixtures** | `F6_kiosk_all_criticals.json` extended |
| **Rust tests added** | +3 (WireGpu wire-honesty) — 1181 → 1184 |
| **Browser gate delta** | 221 → 223 (+2 F6 GPU measured-half assertions) |
| **Contract bumps** | **NONE** (`WireGpu` is repo-local) |
| **New dependencies** | none |
| **Governor touch** | none — the auto-kill-on-thermal wiring is explicitly deferred |
| **Live-verified path** | measured (driver loaded on dev host) |
| **Test-verified path** | unmeasured (three wire-honesty tests + D98 gate `data-testid-unmeasured` on F1/F2/F3) |
