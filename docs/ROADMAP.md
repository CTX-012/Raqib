# edge_monitor — canonical roadmap

> **Standing rule (the lost-plan lesson):** every phase scope doc lives
> in this directory (`docs/`) in this repo. No ephemeral plan files.
> The original Phase 1–5 framing was lost when an off-repo planning
> surface was deleted — costing a full re-anchor cycle (DISPATCH 34
> stopped, DISPATCH 35 reconstructed). This file is the version-
> controlled fix.
>
> **Update via PR with each phase release** so the roadmap and the
> code stay in lock-step (same discipline as
> [docs/PHASE3_DESIGN.md](PHASE3_DESIGN.md)).

This roadmap is built from **verifiable in-repo sources**:

- `git tag -l` + `git log --oneline` (what shipped, when)
- `CHANGELOG.md` (per-version record)
- [`docs/PHASE3_DESIGN.md`](PHASE3_DESIGN.md) (Phase 3 as-shipped)
- `tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_SCOPING.md` +
  `INSPECTOR_PHASE4_IMPL.md` (verified deferred backlog + Phase 4
  impl shape)
- `~/ux_contract/` tags + commit log (the contract version ladder)

Anything **not** verifiable from those sources is marked
**CANDIDATE — not yet scoped**. An honest "unknown" beats a
confabulated plan.

---

## Status legend

| Marker | Meaning |
|---|---|
| **SHIPPED** | Tag exists; CHANGELOG entry present; verifiable in `git log`. |
| **IN PROGRESS** | Scoped + design doc landed; sub-versions partially shipped. |
| **SCOPED** | Operator-decided; design doc landed; no code yet. |
| **CANDIDATE — not yet scoped** | Item in the deferred backlog; no operator commitment to a phase. |
| **EXPLICITLY NOT DOING** | Authority decision not to pursue; documented standing position. |

---

## Shipped (verified against `git tag -l`)

The release ladder, built from `git tag -l | sort -V` against the
v1.2.0 HEAD (`26a84b0`). Versions in **bold** mark phase boundaries.

| Version | Date | What landed | Phase |
|---|---|---|---|
| v0.1.0 | (early) | Prototype tag (pre-v1.0 ladder) | Phase 1 foundation |
| **v1.0.0** | 2026-05-21 | First stable release: core sampling + classifier + UI baseline | **Phase 1 SHIPPED** |
| v1.0.1 | 2026-05-21 | Inspector #1 phantom-kill fix. **`default_ai_action` flipped from `Kill` to `Allow`** — the FIRST observe-only sign-off. Manual `k` keybinding stays. | Phase 1 hotfix |
| v1.0.2 | 2026-05-22 | Small fixes | Phase 1 hotfix |
| v1.0.3 | 2026-05-22 | B-EMPIRICAL VRAM-zero fix (compute-process API); B-EMPIRICAL-4 rclpy detection fix (real markers, not `librclpy.so`) | Phase 1 hotfix |
| v1.0.4 | 2026-05-23 | Final Phase 1 release | Phase 1 wrap |
| v1.1.0 | — | **RETRACTED** (see v1.1.1) | — |
| **v1.1.1** | 2026-05-24 | First usable Phase 2 release: per-category samplers (vLLM, llama.cpp, Ollama, ROS2) | **Phase 2 begins** |
| v1.1.2 | 2026-05-24 | B2 active-detection fix + trait expansion | Phase 2 |
| v1.1.3 | 2026-05-24 | P5 refinements + integration harness (CI deferred) | Phase 2 |
| v1.1.4 | 2026-05-24 | Bug-surface fixes (P5 + DISPATCH 11 carry-forward) | Phase 2 |
| v1.1.5 | 2026-06-01 | Cleanup bundle + BUG-P5-2 | Phase 2 |
| v1.1.6 | 2026-06-01 | Humble-compat hotfix | Phase 2 |
| v1.1.7 | 2026-06-01 | Closed dispatcher clone-pressure leak | Phase 2 (leak saga) |
| v1.1.8 | 2026-06-01 | Partial residual leak fix + STOP-AND-SURFACE | Phase 2 (leak saga) |
| v1.1.9 | 2026-06-01 | **B3 spawn-churn fix — leak saga closed** | Phase 2 (leak saga) |
| v1.1.10 | 2026-06-01 | **ActivityState consume + zombie filter — ghost-row closed** | **Phase 2 SHIPPED** |
| **v1.1.11** | 2026-06-01 | AlertState → Runtime lift (foundation); headless alert logs; second observe-only sign-off | **Phase 3 begins** |
| v1.1.12 | 2026-06-03 | Vitals subsystem: thermal collection + wire + TUI + Svelte (sysfs only; INA3221 deferred); third observe-only sign-off | Phase 3 |
| v1.1.13 | 2026-06-03 | Alerts on the web wire (closes v1.1.11 deferral) — `WireAlertEntry`; fourth observe-only sign-off | Phase 3 |
| **v1.2.0** | 2026-06-03 | **Phase 3 capstone — ranked recommendations** (observe-only). Type firewall (`SuggestedAction: Copy`) + wiring firewall (`tests/recommendation_observe_only_guard.rs`). Fifth observe-only sign-off. | **Phase 3 SHIPPED** |
| **v1.3.0** | 2026-06-03 | **Phase 4 step 1** — `EDGE_MONITOR_THERMAL_ROOT` env override (~35 LoC). Unblocks Jetson-deferred thermal validation on x86 via synthetic sysfs. Forced compat: consumes `ux_contract` v0.3.16 (`HostVitals.power_rails`). | **Phase 4 IN PROGRESS** |

**Total versions on the ladder: 20 (v1.0.0 through v1.3.0, plus
v0.1.0 prototype, minus the retracted v1.1.0).**

### Phase summaries (what each delivered)

- **Phase 1 (v1.0.0 → v1.0.4)** — core sampling + classifier + UI
  baseline. Includes the Inspector #1 phantom-kill flip
  (`default_ai_action = Allow`) and the B-EMPIRICAL fix series
  (VRAM-zero, rclpy detection).
- **Phase 2 (v1.1.1 → v1.1.10)** — per-category samplers + the
  multi-version leak saga (v1.1.7 dispatcher leak, v1.1.8 STOP,
  v1.1.9 spawn-churn closed) + the ghost-row resolution
  (v1.1.10 ActivityState + zombie filter).
- **Phase 3 (v1.1.11 → v1.2.0)** — vitals + alerts + observe-only
  recommendations. Four-step incremental cadence; Inspector audits
  between each step. The release-sequence breakdown lives in
  [`docs/PHASE3_DESIGN.md`](PHASE3_DESIGN.md) as the per-phase
  pattern this roadmap inherits.
- **Phase 4 — IN PROGRESS (v1.3.0 shipped 2026-06-03)** — config-
  driven policy + INA3221 + Jetson hardware pass. v1.3.0 closes the
  smallest-first-ship step (`EDGE_MONITOR_THERMAL_ROOT`) and
  unblocks x86 validation of every prior thermal surface. Sub-
  versions v1.3.1 → v1.3.3 remain scoped per
  [`docs/PHASE4_DESIGN.md`](PHASE4_DESIGN.md).

### `ux_contract` ladder (the shared contract crate)

The Agent A producer crate that both Linux and (paused) Windows
binaries consume via path dep. Tag set: `v0.3.0` through `v0.3.15`.

| Contract version | Provides | Consumer phase |
|---|---|---|
| v0.3.0 → v0.3.12 | Initial UX contract, status/empty/confirm/errors/alerts strings, AlertId, ActivityState (v0.3.12) | Phase 1 / 2 baseline |
| v0.3.13 | `HostVitals` + `ThermalZone` + `thresholds::THERMAL_AMBER_C` (85) / `THERMAL_RED_C` (95) | v1.1.12 vitals |
| v0.3.14 | `Recommendation`, `SuggestedAction` (Copy firewall), `RecommendationScope`, `RecommendationSeverity`, `RecommendedTarget`, `display::*` templates, `REC_MAX_VISIBLE` = 3, `REC_TARGETS_MAX` = 3, `RECOMMENDATION_NOT_ACTIONABLE` | v1.2.0 capstone |
| v0.3.15 | `AlertId::ThermalPressure` (template + tier) | v1.2.0 capstone (consumed alongside v0.3.14 surface) |
| v0.3.16 | `HostVitals.power_rails: Vec<PowerRail>` + `PowerRail` struct (INA3221 type surface) | v1.3.0 (forced compat — empty rails vec) + v1.3.3 (collection) |

The contract crate's authority-lock primitive lives at v0.3.14:
`SuggestedAction` is `Copy` and has no method-on-value, pinned by
`suggested_action_is_copy` in the contract's test suite. The
consumer-side mirror is
[`tests/recommendation_observe_only_guard.rs`](../tests/recommendation_observe_only_guard.rs).

---

## Phase 4 — IN PROGRESS (v1.3.0 shipped)

**Status: IN PROGRESS** — v1.3.0 shipped 2026-06-03; v1.3.1–v1.3.3
remain scoped. Canonical design lives at
[`docs/PHASE4_DESIGN.md`](PHASE4_DESIGN.md).

**Scope (locked by operator at DISPATCH 47 §7 + reaffirmed by
DISPATCH 48 §1 authority line):**

- **Config-driven policy** — per-workload rule layer + deployment
  threshold overrides.
- **INA3221 per-rail power on Jetson** — closes the v1.1.12 thermal
  deferral.
- **`EDGE_MONITOR_THERMAL_ROOT` env override** — unblocks x86 testing
  of the thermal-alert + recommendation path.
- **Jetson hardware pass** — empirical validation on actual Orin (live
  thermal recs, multi-zone, amber/red rendering, INA3221).

**Cadence**: incremental v1.3.x sub-versions (mirrors Phase 3's
v1.1.11 → v1.2.0 pattern). Per
[`INSPECTOR_PHASE4_IMPL.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_IMPL.md)
§6, the proposed sequence:

| Sub-version | Scope | Size | Contract prereq | Status |
|---|---|---|---|---|
| **v1.3.0** | Thermal env override (`EDGE_MONITOR_THERMAL_ROOT`) | ~35 LoC + forced v0.3.16 compat | `ux_contract` v0.3.16 (landed parallel) | **shipped 2026-06-03** |
| v1.3.1 | `[thresholds]` + `[samplers]` deployment overrides | ~150-250 LoC | Q1 HYBRID locked (PHASE4_DESIGN §3) | scoped |
| v1.3.2 | `[[workloads]]` per-workload rules + suppression flags | ~200-300 LoC | none beyond v1.3.1 | scoped |
| v1.3.3 | INA3221 power rails | ~180 LoC consumer | `ux_contract` v0.3.16 (already landed) | scoped |
| Jetson pass | Empirical validation on Orin | empirical-only | none | scoped (post-v1.3.3) |

**Total estimate**: ~600-800 LoC + Jetson hardware pass.

**Authority lock (binding — the SIXTH explicit reaffirmation):**
Phase 4 is OBSERVE-ONLY. Every Phase 4 element makes
thresholds/intervals/rules **tunable**; none adds **act-on-rule**.
The `[[workloads]]` schema has NO `action_on_breach` field, NO
`auto_kill` field, NO `priority` field — schema-level firewall.
Even an operator editing TOML cannot configure auto-action because
the field doesn't exist. The authority audit at
[`INSPECTOR_PHASE4_IMPL.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_IMPL.md)
§8 enumerates every proposed Phase 4 element and confirms each is
observation- or display-side.

**Design doc**: canonical Phase 4 design lives at
[`docs/PHASE4_DESIGN.md`](PHASE4_DESIGN.md) (landed with v1.3.0 per
DISPATCH 50). The
[`INSPECTOR_PHASE4_IMPL.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_IMPL.md)
report remains the verbose impl-shape reference; the design doc
captures the locked operator decisions (Q1–Q7) verbatim.

**Operator decisions (LOCKED at DISPATCH 47 §7, DISPATCH 49,
DISPATCH 50)** — full text in
[`docs/PHASE4_DESIGN.md`](PHASE4_DESIGN.md) §3:

1. Contract-vs-config tension: **Option (iii) HYBRID**. Wire caps
   absolute; deployment thresholds become defaults
   (config-overridable); implementation thresholds absolute.
2. Per-workload match shape: **EXACT name**. Regex/glob deferred
   to v1.4.x if needed.
3. Suppression flags: **BOTH** `suppress_alerts` and
   `suppress_recommendations`, independently togglable.
4. Per-workload fields beyond thresholds + suppress: **DEFER to
   v1.4.x**.
5. Agent A dispatch for `ux_contract` v0.3.16: **fired parallel,
   already landed**.
6. Jetson pass owner: **Tester** (or operator hand-off at v1.3.3
   ship).
7. Sub-version cadence: **INCREMENTAL** (v1.3.0 → v1.3.3).

---

## Candidate / future — honest backlog (NOT scoped)

These items are in the deferred backlog (verified against
[`INSPECTOR_PHASE4_SCOPING.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_SCOPING.md)
§3) but have no operator commitment to a phase. They become
candidates for v1.4.x or later only when the operator explicitly
scopes them — **not** by silent assumption.

### Bounded in-repo items

| Item | Source | Size estimate |
|---|---|---|
| Inspector #15 cache-clear gap follow-up | [`src/telemetry/source.rs:351`](../src/telemetry/source.rs#L351) | ~30-60 LoC |
| Wide-tier two-column workload-selection focus | [`src/ui/panels/mod.rs:264`](../src/ui/panels/mod.rs#L264) | ~40-80 LoC |
| Sprint-7 Item 5 — Ollama spawn race | [`BACKLOG.md:9`](../BACKLOG.md#L9) | unknown — needs live repro first |

### Items requiring operator scope decision

- **ROS2 Hz sampling** — `DESIGN_HANDOFF.md:109, :1248, :1306` mark
  Hz as "deferred to v1.1." The v1.1.x line completed without
  picking it up. **Status (DISPATCH 50 annotation): deprecated by
  the echo-once probe approach the L13 ros2_shellout sampler
  ships** (process-level RAM/CPU + activity state + echo-once
  probes for cadence proxies). Hz-per-topic would need a proper
  rclrs binding to attach a subscription per node — that's a
  bigger lift than the original "v1.1 Hz" deferral implied.
  **Revivable** if/when an operator scopes the rclrs path; until
  then it stays in this candidate list rather than as a binding
  deferral. (No operator drop call this dispatch — left as honest
  "open, not in active scope.")

### Future contract surface (CANDIDATE)

- `ux_contract` v0.3.16 — INA3221 power rails. Needed for Phase 4
  v1.3.3 above; Agent A dispatch fires when operator confirms
  impl §9 Q5.
- Anything beyond v0.3.16 is speculative — not listed here to avoid
  false specificity.

### Why this list is not "Phase 5"

The previous framing (per session memory only — verified absent
from repo at
[`INSPECTOR_PHASE4_SCOPING.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_SCOPING.md)
§2) named "Phase 4 = web control" and "Phase 5 = history." Both
labels are **obsolete against current source**:

- "Web control": shipping a kill button on the web UI crosses the
  authority line every prior phase explicitly held (see EXPLICITLY
  NOT DOING below). Shipping passive web display: already done
  (v1.1.13 alerts + v1.2.0 recs).
- "History": the RunStore + `edge_monitor history` CLI already
  exist (per CHANGELOG history entries dating back to v0.x); the
  v1.1.x "richer history" items (sharing/export/tagging) were
  formally **dropped** this dispatch — see below.

The repo does not name Phase 5. This roadmap will not invent one.
When the Phase 4 v1.3.x release completes, operator decides what
Phase 5 (if any) is **from the candidate list above + whatever new
operator priority emerges**, and that decision lands as a
`docs/PHASE5_DESIGN.md` doc in this repo.

---

## EXPLICITLY NOT DOING

The authority position, in writing.

### Governor actuation — separate, deliberate, operator-only decision

Automated actuation of any kind (auto-kill on threshold breach,
tick-path `send_sigterm` wiring, `--enable-governor` flag) is **NOT
a planned phase.** The observe-only line has been held with
**explicit operator sign-off EIGHT times**:

1. **v1.0.1** — Inspector #1 phantom-kill bug → `default_ai_action`
   flipped from `Kill` to `Allow`; the FIRST authority pin.
2. **v1.1.11** — Phase 3 step 1 sign-off (AlertState lift); reaffirmed.
3. **v1.1.12** — Phase 3 step 2 sign-off (vitals subsystem);
   reaffirmed.
4. **v1.1.13** — Phase 3 step 3 sign-off (alerts on web wire);
   reaffirmed.
5. **v1.2.0** — Phase 3 capstone (recommendations) added a TYPE-LEVEL
   firewall (`SuggestedAction: Copy`) AND a wiring-level firewall
   (`tests/recommendation_observe_only_guard.rs`); reaffirmed.
6. **DISPATCH 41** — Phase 4 scoping pre-pass authority confirmation.
7. **DISPATCH 47** — Phase 4 implementation pre-pass authority
   confirmation; `[[workloads]]` schema designed without an `action_
   on_breach` field as a schema-level firewall.
8. **v1.3.0 / DISPATCH 50** — Phase 4 step 1 sign-off
   (`EDGE_MONITOR_THERMAL_ROOT`); env override is pure observation-
   path config, no actuation surface added.

**Standing position**: automated actuation is a separate, deliberate
decision requiring its **own dedicated safety-design track**
(distinct dispatch, distinct doc, dedicated Inspector audit on the
authority-expansion question, separate operator sign-off on the
expansion itself, not just on the design). It is opened only by
**explicit operator choice**, not by implementation drift.

If actuation is ever pursued, it needs (per
[`INSPECTOR_PHASE4_SCOPING.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_SCOPING.md)
§5):

- Operator sign-off on **opening the question** of crossing the line.
- Dedicated safety-design phase (separate dispatch / doc / Inspector).
- Reversibility tracking (pre-kill grace + operator cancel).
- Kill-switch (single-key disable of all automated actuation).
- Per-workload opt-in (Phase 4 config rules are a prerequisite).
- Audit-first design (every decision logged BEFORE signal sent).

Manual actuation via the `k` → `kill_confirm` card → SIGTERM path is
the only authorized actuation surface and is **unchanged** since
v1.0.0.

### Dropped stale deferrals (operator-decided, DISPATCH 47 §7 + DISPATCH 49)

The following items appeared as "deferred to v1.1" in
`DESIGN_HANDOFF.md` (and a small set of in-source `// deferred to
v1.1` markers) but were never picked up across the entire v1.1.x
line. Operator formally dropped them this dispatch:

| Item | Original source line | Resolution |
|---|---|---|
| `/` filter UX | [`src/ui/app.rs:46`](../src/ui/app.rs#L46), [`:459`](../src/ui/app.rs#L459) (deferred-to-v1.1 comments) | **DROPPED** — v1.1.x line completed without it. The `// L2c removed` comment in `app.rs` stays as historical context. |
| Sharing/export | `DESIGN_HANDOFF.md:37`, `:370` (§11), `:760` | **DROPPED** — `SHARING_SPEC.md` was forthcoming and never landed. |
| Tagging | `DESIGN_HANDOFF.md:38`, `:760` | **DROPPED** — never picked up. |
| Notifications | `DESIGN_HANDOFF.md:38`, `:760` | **DROPPED** — never picked up. |
| Custom themes | `DESIGN_HANDOFF.md:38`, `:760` | **DROPPED** — three themes (dark/light/hc) are sufficient; no operator ask for user-defined themes emerged. |

`DESIGN_HANDOFF.md` is cleaned in this same commit to strike the
five stale deferral lines and mark §11 as DROPPED with a pointer to
this roadmap.

**Stale deferrals NOT on the drop list — still open**:

- **ROS2 Hz sampling** (`DESIGN_HANDOFF.md:109, :1248, :1306`) —
  remains "deferred to v1.1" and was not on the operator's
  DISPATCH 47 drop list. Surfaced as a CANDIDATE above pending
  operator drop/keep decision.

### Other "out of scope" items (carry-over from v0.3 contract)

The `DESIGN_HANDOFF.md` §0 out-of-scope list (lines 28-38) was
authored against the v0.3 contract design and predates the v1.0.1
authority-lock flip. The line "**Auto-kill on resource pressure —
governor fires only on user-defined `[[workloads]]` rules**" describes
the v0.3 design-time intent, not the current standing position; the
**current** position is the seven-reaffirmation observe-only lock
above. Phase 4's `[[workloads]]` rule shape per
[`INSPECTOR_PHASE4_IMPL.md`](../tests/empirical/audit_2026-06-01/INSPECTOR_PHASE4_IMPL.md)
§5 deliberately **excludes** the action-on-breach fields that v0.3
design-time intent assumed; the rules are observe/recommend
tuning, not auto-kill triggers.

Other §0 lines stay as-is (ROS1 detection, stderr persistence,
historical analysis beyond 20 runs, Prometheus-as-fleet-bridge,
TUI-not-task-manager).

---

## Process rules (the lessons, in writing)

1. **Plan-doc discipline.** Every phase scope doc lives in `docs/`
   in this repo. No ephemeral plan files. The original 5-phase
   framing was lost when an off-repo planning surface was deleted;
   this roadmap is the fix. **Repeat the loss costs another
   re-anchor cycle.**
2. **Roadmap updated via PR each phase release.** Design + code
   stay version-controlled in sync. Same discipline as
   `docs/PHASE3_DESIGN.md` (updated at each v1.1.11 / v1.1.12 /
   v1.1.13 / v1.2.0 sub-release).
3. **Repo is ground truth.** Memory of "the plan" is useful
   context, never authoritative. When this roadmap and an
   off-repo memory disagree, the roadmap wins; surface the
   discrepancy for explicit reconciliation rather than silently
   drifting.
4. **Phase scope decisions are operator-side.** Inspector pre-passes
   enumerate candidates; the operator chooses. No phase auto-
   promotes from "candidate" to "scoped" by implementation drift.
5. **Authority expansion is its own decision track.** Crossing the
   observe-only line (governor actuation) is a SEPARATE decision
   from any phase scope; it requires explicit operator sign-off on
   opening the question, a dedicated safety-design doc, and a
   dedicated Inspector audit.
6. **`ux_contract` changes go through Agent A.** Per CLAUDE.md's
   "Multi-agent workflow": LinuxImpl files a Contract Amendment
   Request, not an edit of `~/ux_contract/`. The v0.3.13 → v0.3.14 →
   v0.3.15 ladder is the pattern Phase 4 v1.3.3 inherits.

---

## Document history

| Date | Change |
|---|---|
| 2026-06-03 | Initial author (DISPATCH 49). Built from `git tag -l` + CHANGELOG + Inspector Phase 4 reports + DESIGN_HANDOFF. Phase 4 scope: SCOPED. Five stale deferrals: DROPPED. |
| 2026-06-03 | Updated for v1.3.0 ship (DISPATCH 50). Phase 4: IN PROGRESS. v1.3.0 shipped row added; ux_contract v0.3.16 row added. Phase 4 design promoted to `docs/PHASE4_DESIGN.md`. Operator decisions Q1-Q7 marked LOCKED. ROS2-Hz annotated as deprecated-by-echo-once / revivable-with-rclrs. Observe-only sign-off count: 7 → 8. |
