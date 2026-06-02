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

| Release | Scope | Dependency on `ux_contract` |
|---|---|---|
| **v1.1.11** (this release) | AlertState → Runtime (foundation) + this design doc | None |
| **v1.1.12** | Vitals-driven alert eval (thermal, power, throttling) | v0.3.13 (vitals types) |
| **v1.2.0** | Ranked recommendations surface | v0.3.14 (recommend templates) |

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

### v1.1.12 — vitals (in flight, depends on `ux_contract` v0.3.13)

The vitals types — `Temp`, `Power`, `Throttling` — land in
`ux_contract` v0.3.13 (Agent A's parallel scope as of the
DISPATCH 35 / DISPATCH 36 split). Once v0.3.13 ships, v1.1.12
adds:

- Vitals reads on the platform layer (NVML temp / power on
  RTX; `/sys/class/thermal/` on Jetson Orin; INA3221 deferred).
- Vitals-driven alerts at the **operator-locked thresholds**:
  **85 °C amber** (Attention) / **95 °C red** (Critical).
- The web wire grows an `alerts: Vec<AlertEntry>` list (the
  wire-type-needs-`ux_contract` blocker the v1.1.11 dispatch
  surfaced).

INA3221 (per-rail power on Jetson) is **deferred** — the
v1.1.12 reads cover NVML and `/sys/class/thermal/` only.

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
| v0.3.13 | Vitals types (`Temp`, `Power`, `Throttling`, vitals alert IDs) | in flight (Agent A) |
| v0.3.14 | Recommendation templates + ranking enum | held until v1.2.0 surface designed |

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
