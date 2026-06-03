# Phase 3 Design — vitals + governor-finish

Status: **locked scope** as of v1.1.11 (2026-06-01). Inspector
DISPATCH 35 ratified by operator. This file replaces the
ephemeral session-storage planning doc that vanished when its
process changed; the canonical Phase 3 scope now lives in
version control alongside the code it governs.

> If you came here looking for the v0.x design docs, see
> [DESIGN_HANDOFF.md](../DESIGN_HANDOFF.md). That file remains
> the locked source of truth for UX_CONTRACT.md v0.3 and the
> Linux L1–L26 implementation plan; this file is strictly the
> Phase 3 scope add-on.

---

## 1. What Phase 3 IS

Phase 3 is **Vitals + Governor-finish**. Two halves:

1. **Vitals** — system-health observation surface: temperature
   reads, power draw, throttling indicators, alert eval and
   surfacing on every UI mode (TUI / headless / web). The reads
   themselves are observation-only; nothing in Phase 3 takes a
   policy action based on them.
2. **Governor-finish** — close the gaps left by v1.0.1's
   "automated governor disabled by default" landing
   ([CLAUDE.md known limitations](../CLAUDE.md)). The manual
   `kill_confirm` card path stays as-is; automated kill wiring
   stays UN-WIRED. Phase 3 finishes the **infrastructure** that
   would let automated actuation eventually exist, without
   actually turning it on.

## 2. What Phase 3 is NOT

**Authority lock (binding, operator sign-off):**

- **OBSERVE-ONLY.** No automatic actuation. No tick-path kill
  wiring. No `--enable-governor` flag. `send_sigterm` stays
  manual-only.
- **`default_ai_action = Allow` UNCHANGED.** v1.0.1's opt-in
  posture holds; nothing in Phase 3 flips it.
- **Config-driven policy is DEFERRED.** Out of Phase 3 entirely —
  belongs to a later release after the observe-only surfaces
  are validated.

Any temptation to wire actuation in this phase: STOP, surface.
Operators get observation, alerts, and recommendations; they
keep the trigger.

## 3. Build sequence

Three incremental releases, each independently bisectable:

| Release | Scope | Dependency on `ux_contract` | Status |
|---|---|---|---|
| **v1.1.11** | AlertState → Runtime (foundation) + this design doc | None | **shipped** |
| **v1.1.12** | Vitals subsystem (thermal collection + wire + TUI + Svelte) | v0.3.13 (`host_vitals` + `thermals::THERMAL_*_C`) | **shipped** |
| **v1.1.13** | Alerts on the web wire (`WireAlertEntry`) — closes v1.1.11 deferral | v0.3.13 (`AlertId` + `alerts::*` templates already present) | **shipped** |
| **v1.2.0** | Ranked recommendations surface + thermal alert firing | v0.3.14 (recommend templates) | upcoming |

### v1.1.11 — AlertState lift

The foundation. AlertState was TUI-only in v1.0.x: it lived on
`App` (`src/ui/app.rs:86`), and `--no-ui` headless mode never
constructed `App` → alerts silently dropped. The lift moves
ownership to `RuntimeState::alerts` so the eval fires on every
tick regardless of UI mode.

Headless emission lands here too: each visible alert generates
one `INFO`-level `tracing` line with `alert.fire=<AlertId>`,
grep-able by journald / vector / etc.

Per the dispatch's WIRE NOTE: the web wire does NOT grow an
`AlertEntry` list in v1.1.11 — that requires a `ux_contract`
type and is properly v1.1.12+. v1.1.11 ships headless **logs**
only.

### v1.1.12 — vitals subsystem (shipped; depends on `ux_contract` v0.3.13)

Shipped 2026-06-03 via DISPATCH 39. Consumes
`ux_contract::host_vitals::{HostVitals, ThermalZone}` and
`ux_contract::thresholds::{THERMAL_AMBER_C, THERMAL_RED_C}`.

What landed:

- **Platform-layer collection** (`src/platform/host_vitals.rs`):
  reads `/sys/class/thermal/thermal_zone*/{type,temp}` via
  `std::fs`. Per-zone errors silently skipped (RAPL "unreadable →
  None" pattern). `cooling_device*` siblings filtered out.
  Labels stably sorted at the producer. Empty `thermal_zones`
  means "no zones discovered"; consumer hides the panel.
- **Web wire** (`src/web/wire.rs`): `WireThermalZone { label,
  temp_celsius, severity }` and `WireThermalSeverity { Nominal,
  Amber, Red }`. Severity pre-classified server-side via
  `classify_thermal` against the contract thresholds — single
  source of truth; the TUI and Svelte each read the contract
  directly, no `>= 85` literals in either consumer.
  `#[serde(default)]` makes the `thermal_zones` field
  backward-compat additive.
- **TUI** (`src/ui/panels/vitals.rs`): top-3 hottest zones on a
  new 5th row of the existing vitals panel; "N of M zones shown"
  hint when there are more. Hidden when empty. Colored by the
  hottest zone's tier.
- **Svelte** (`web/src/components/VitalsPanel.svelte`): matches
  TUI's top-3 + count behaviour so both surfaces present the
  same hottest zones in the same order. `text-critical` /
  `text-attention` / `text-fg` mapping driven by the
  server-classified severity variant; NO numeric thresholds in
  TypeScript.

**Explicit non-actions** (authority lock):

- NO alert variant for thermal added to `AlertState`. v1.1.12 is
  display only.
- NO actuation, NO `send_sigterm` wiring, NO `--enable-governor`
  flag. `default_ai_action = Allow` unchanged.

Deferred to v1.2.0+ (per DISPATCH 39 scope lock):

- Thermal-driven alert FIRING (the lift is foundation; alerts
  fire when the recommendations surface lands).
- INA3221 per-rail power on Jetson.
- NVML temperature / power reads (the v1.1.12 reads are sysfs
  only; NVML follows as a separate platform-layer addition).

### v1.1.13 — alerts on the web wire (shipped; closes v1.1.11 deferral)

Shipped 2026-06-03 via DISPATCH 42. Consumes `ux_contract`
v0.3.13 (the existing `AlertId`, `alerts::*` templates, and
`alert_tier` mapping cover everything the wire surfaces). NO new
contract type needed — Inspector DISPATCH 41 confirmed.

Closes the v1.1.11 deferral. v1.1.11 lifted `AlertState` to
`Runtime` so the eval fires on every tick regardless of UI mode,
and added `log_visible_alerts` for headless tracing. It DEFERRED
the web-wire surface ("needs a `ux_contract` type"). DISPATCH 41
re-read found that v0.3.13's surface was already sufficient and
v1.2.0 (ranked recommendations) rides on alerts-on-wire, so the
deferral had to close before the capstone.

What landed:

- **`WireAlertEntry`** (`src/web/wire.rs`): `{ alert_id,
  pid, workload_name, severity, text }`. The `alert_id` is the
  snake-case projection of `ux_contract::AlertId`; `severity` is
  the snake-case projection of `crate::ui::panels::alerts::AlertTier`
  (`'attention' | 'critical'`); `text` is the byte-for-byte
  rendering the TUI banner shows, produced server-side via the
  SAME `panels::alerts::substitute(template_for(id), entry,
  live_values_for(entry, state))` pipeline.
- **`WireSnapshot.alerts: Vec<WireAlertEntry>`** with
  `#[serde(default)]` — backward-compat additive (same shape as
  `thermal_zones` in v1.1.12 and `activity_state` in v1.1.10).
- **`AlertsPanel.svelte`** rendering the wire alerts: pill-shaped
  lines, colored by the server-classified severity
  (`bg-critical` / `bg-attention`), positioned above the main
  panel grid so visible alerts catch the operator's eye first.
  Self-hides when no alerts are visible.
- **Cross-layer hygiene**: `panels::alerts::template_for` widened
  from private to `pub(crate)` for the wire builder. Single call
  site (one line in panels/alerts.rs); the visibility bump is
  annotated with a v1.1.13 / DISPATCH 42 comment.

**Explicit non-actions** (authority lock):

- NO new alert IDs (ThermalPressure stays v1.2.0+ scope).
- NO recommendation structure (also v1.2.0+).
- NO web-side ack flow (ack is a TUI keybinding today; web ack
  is v1.2.0+ scope and was not asked for in this dispatch).
- NO actuation surface.

### v1.2.0 — recommendations (depends on `ux_contract` v0.3.14)

Ranked recommendations: instead of a single alert firing per
breach, the surface presents the operator with a ranked list
("kill this", "reduce batch size", "wait it out"). Templates +
ranking logic land in `ux_contract` v0.3.14, held until the
v1.2.0 UI surface design is locked.

This is the FIRST phase where the surface is something more
opinionated than "here's a fact about the system." Even so,
the operator still pulls the trigger — Phase 3 stays
observe-only.

## 4. `ux_contract` prereqs

| Contract version | Provides | Status |
|---|---|---|
| v0.3.12 | `ActivityState` (lifted in v1.1.10) | shipped |
| v0.3.13 | `host_vitals::{HostVitals, ThermalZone}` + `thresholds::THERMAL_AMBER_C` (85.0) / `THERMAL_RED_C` (95.0). Consumed by v1.1.12. | shipped |
| v0.3.14 | Recommendation templates + ranking enum + thermal alert IDs | held until v1.2.0 surface designed |

If a v1.1.11+ implementation step reveals a contract gap
(missing string, alert ID, threshold, type), the dispatch must
file a Contract Amendment Request rather than invent a type
locally. The Agent A / Agent B separation from CLAUDE.md's
"Multi-agent workflow" still holds.

## 5. Thermal thresholds

Operator-locked:

- **85 °C → Attention tier** (amber banner). At 85 °C the GPU
  has begun throttling on most NVIDIA SKUs the project targets
  (3060 / 3080 / Orin); the operator should be alerted before
  performance falls off the floor.
- **95 °C → Critical tier** (red banner). 95 °C is the published
  thermal-shutdown threshold on the consumer SKUs; surfacing it
  at this point gives the operator time to intervene before the
  kernel TDP-clamps or the driver resets the device.

The thresholds will live as `ux_contract::thresholds::TEMP_*_C`
constants in v0.3.13.

## 6. Non-goals

Explicitly out of Phase 3 (named so a future PR can't quietly
expand scope):

- **Automatic actuation of any kind** (the authority lock above).
- **Config-driven policy / per-workload rules** — deferred to a
  later release.
- **INA3221 per-rail power on Jetson** — deferred; v1.1.12
  covers NVML + `/sys/class/thermal/` only.
- **Workload-level recommendation execution** — v1.2.0 ranks
  and presents; the operator dispatches.

## 7. Process notes

- This file is the canonical Phase 3 scope. **Update it via PR
  with each Phase 3 release** so the design doc and the code
  stay in version-control sync. The ephemeral planning surface
  that this file replaces did not, and the loss was non-trivial.
- The DISPATCH 36 / v1.1.11 commit history is the authoritative
  trail for the AlertState lift; see `CHANGELOG.md [1.1.11]` for
  the per-ITEM breakdown.
