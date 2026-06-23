# edge_monitor — History Subsystem Design (Phase 5)

**Status:** design draft 2026-06-23 · promotes to a `docs/PHASE5_DESIGN.md` sub-section at sign-off
**Basis:** DISPATCH 88 Inspector read-only pass against HEAD `v1.3.2-15-g7bba388`
**Scope (operator-ratified):** in-memory ring + per-PID trajectories + cross-PID event timeline. No disk persistence beyond what `RunStore` already does. First consumer: a history view (web-first, TUI follow-up).

## Architecture position

```
              ┌─────────────────────────────────────────────────────┐
              │  Phase 1-2:  ResourceStats (peaks-only, per-PID)    │  ← exists today
              │              record_sample() at lifecycle/mod.rs:115 │
              └─────────────────────────────────────────────────────┘
                                       │
                                       ▼
              ┌─────────────────────────────────────────────────────┐
              │  Phase 4:  ThresholdBreach + decisions              │  ← exists today
              │            evaluate() / actuation (D78-D84)         │
              └─────────────────────────────────────────────────────┘
                                       │
                                       ▼
              ┌─────────────────────────────────────────────────────┐
 NEW IN P5 ⇒  │  History:  in-memory session window                 │
              │   - per-PID trajectories (rolling time-series)      │
              │   - cross-PID event archive (longer than feed cap)  │
              │   - snapshot-on-open view (web → TUI)               │
              └─────────────────────────────────────────────────────┘
                                       │
              ┌────────────────────────┼────────────────────────────┐
              ▼                        ▼                            ▼
       History VIEW           Post-mortem shape-B              RunStore (disk)
       (first consumer)       (second consumer; D74 deferred)  (peaks only;
       Q5 below               Q6 below                          unchanged)
```

## What already exists (the grounding — every number cited here is read from the code)

| Surface | Where | Bound | Today's role |
|---|---|---|---|
| `ResourceStats { cpu_sum_pct, cpu_peak_pct, rss_peak_bytes, vram_peak_bytes, sample_count }` | [lifecycle/mod.rs:21-27](../src/lifecycle/mod.rs#L21) | static (one per `ProcessLifecycle`) | rolling **aggregates** — discards individual samples |
| `ProcessLifecycle::record_sample(cpu, rss, vram)` | [lifecycle/mod.rs:115](../src/lifecycle/mod.rs#L115) | called from runtime per-tick | folds into ResourceStats; **no time series retained** |
| `Runtime::tick()` → `record_sample` driver | [runtime.rs:981-993](../src/runtime.rs#L981) | 1 Hz default | the sampling moment we hook into for trajectories |
| `RunRecord { summary, metrics, exit_reason, cold_start, ... }` | [storage/run_store.rs:228-257](../src/storage/run_store.rs#L228) | persistent (RunStore JSONL) | **peaks only** — no trajectory field |
| `state.completed: VecDeque<LifecycleSummary>` | [runtime.rs:115](../src/runtime.rs#L115) | `runtime.completed_history` (default **50**, [config.rs:502](../src/config.rs#L502)) | live in-memory ring of exits; activity feed source |
| `state.audit: VecDeque<AuditLogEntry>` | [runtime.rs:120](../src/runtime.rs#L120) | `runtime.audit_history` (default **100**) | live in-memory ring of kills (manual + auto); activity feed source |
| `state.regressions: VecDeque<RegressionEvent>` | [runtime.rs:123](../src/runtime.rs#L123) | `runtime.audit_history` (default **100**) | live in-memory ring of regressions; activity feed source |
| `state.recent_exit_attribution: VecDeque<Option<ExitAttribution>>` | [runtime.rs:119](../src/runtime.rs#L119) | lock-step with `state.completed` | D74 attribution; pushed in lock-step ([runtime.rs:1105-1108](../src/runtime.rs#L1105)) |
| `build_activity(state)` (web wire projection) | [web/wire.rs:1254](../src/web/wire.rs#L1254) | truncates to `ACTIVITY_FEED_WIRE_MAX = 50` | merges the 3 sources above, time-desc |
| `build_events(state)` (TUI activity panel) | [ui/panels/activity.rs:171](../src/ui/panels/activity.rs#L171) | TUI caps at `ACTIVITY_FEED_TUI_MAX = 5` | same merge, narrower display cap |
| Activity caps in contract | [ux_contract `limits`](../../ux_contract/src/lib.rs) | TUI=5, WEB=12, WIRE=50 | the wire is already capped |
| Per-model "history overlay" (TUI `h` key) | `App::open_history` at [ui/app.rs:568](../src/ui/app.rs#L568), backed by `Runtime::history(model, n)` at [runtime.rs:843](../src/runtime.rs#L843) | RunStore-backed (disk) | the **existing** per-model history surface — NOT what this dispatch builds |

### The surprise (call it out)

**Trajectories don't exist anywhere today.** [`ResourceStats::record`](../src/lifecycle/mod.rs#L30) folds each tick into `cpu_sum_pct` + per-resource peaks and increments `sample_count`. The individual samples are dropped on the floor. So Phase 5 has to *create* the per-PID time series — it's not a matter of exposing data the platform already collects.

The TUI `h` key (`KEY_HISTORY` in [ux_contract:540](../../ux_contract/src/lib.rs#L540)) already binds to a "history overlay" that surfaces RunStore peaks per model. That's a different surface from the cross-PID time-ordered History view this dispatch designs. Both will coexist; the dispatch's view is **not** a replacement for the per-model overlay.

## Locked decisions (Q1–Q6)

| Q | Decision | Rationale |
|---|---|---|
| **Q1 — One subsystem or two?** | **ONE `History` module with TWO bounded structures inside.** `History { trajectories: HashMap<PID, TrajectoryRing>, event_archive: VecDeque<HistoryEvent> }`. The unified namespace is operator-facing (one /api/history endpoint, one view); the two collections are structurally distinct because they have orthogonal access patterns (per-PID write-then-query vs. cross-PID write-then-time-query). | Trajectory is high-volume per-PID write (~14×1Hz on the operator's host) and queried by PID. Event archive is low-volume cross-PID and queried time-descending. Forcing them into one structure means either a big-and-wasteful unified ring (every entry carries PID indirection for events that don't have one) or a single VecDeque ordered by time but indexed for per-PID queries (re-implements the HashMap). Two structures inside one module is the honest shape. |
| **Q2 — Ring bound for trajectories** | **Per-PID rolling 1800-sample buffer (≈ 30 min @ 1 Hz)**, sized for the operator's hot path. With 32 live AI PIDs as a generous worst case and a 32-byte sample (timestamp+cpu+rss+vram, see math below), worst-case memory = **32 × 1800 × 32 = 1.76 MB**. Configurable via `runtime.history_trajectory_samples_per_pid` (default 1800). | A wall-clock window would lose dead processes the moment they pass the cutoff; a sample-count cap retains the LAST N samples per PID regardless of age — perfect for post-mortem reads. 30 min is the empirical "how far back is interesting" window operators have asked for on this host. The 32-PID worst case is 4× headroom over the 8 PIDs the operator's ROS2 + ollama + python3 workload runs. |
| **Q3 — Dead-process retention** | **On PID exit, MOVE the trajectory into a new optional field on `LifecycleSummary`** (`trajectory: Option<Trajectory>`). The dead PID's entry is removed from `History.trajectories`; the trajectory now lives with the `LifecycleSummary` in `state.completed` (in-memory, bounded by `completed_history` = 50). `RunStore` (disk) keeps records peaks-only — trajectory is NOT persisted, matching the operator's "lost on restart" scope. **THIS IS THE LOAD-BEARING DECISION; see "Q3 in detail" below for the alternatives weighed.** | Trajectories naturally follow the lifecycle of the process — alive ⇒ in `History.trajectories`, dead ⇒ on the `LifecycleSummary`. No separate "recently-dead" eviction machinery (which would have its own retention math + lock); the existing `completed_history` cap already evicts old dead processes for free. Post-mortem shape-B (D74 deferral) reads `state.completed[i].trajectory` directly — no new lookup path. |
| **Q4 — Event timeline vs the existing activity feed** | **Additive: a new `History.event_archive: VecDeque<HistoryEvent>` bounded at 500 entries** (~150 KB at ~300 B/entry). Receives the same events the existing 3 ring sources do — pushed once at the runtime exit-drain / kill-record / regression-record site (existing call sites; no new wire). Wire and TUI activity feed caps are UNCHANGED (50/12/5); they keep reading from `state.completed`/`audit`/`regressions`. The history view queries the larger archive. | Raising the existing caps would bloat every `/api/snapshot` payload (the activity field is serialized every tick). An additive archive doesn't disturb the live wire — it's only read on demand via the new /api/history endpoint. 500 entries covers ~1 hour of busy operator activity (15-30 events/min × 30+ min). |
| **Q5 — View: live-stream or snapshot?** | **SNAPSHOT ON OPEN.** When the operator opens the history view, the server returns the History state at that moment; subsequent ticks don't shift entries under the viewer. A "Reload" button (web) / `r` key (TUI) explicitly refreshes. **Web first** (mirrors the D74 post-mortem precedent); TUI follow-up. | The D76 selection-stability problem applies here in spades — at 1 Hz the rolling buffer would drop the operator's selected sample as they tried to read it. Snapshot semantics are simpler to reason about and consistent with `/api/snapshot`'s read-only model. Web-first because: (a) the view is naturally a chart surface (Svelte SVG/canvas already trivially renders curves; ratatui needs more work for time-series), (b) post-mortem shape-B was always conceived as a web extension, (c) the existing TUI `h` overlay already exists for the per-model surface, so the TUI history affordance is partly served. |
| **Q6 — Integration with what exists** | **History sits ALONGSIDE `RunStore`** (different schemas, different lifetimes — RunStore is disk-persistent peaks; History is in-memory session window with trajectories). **Post-mortem shape-B is the second consumer** of History, reading the trajectory off the `LifecycleSummary` that the existing card already projects (no new lookup). **The existing TUI `h` overlay (per-model RunStore browser) STAYS** — it's a different surface (per-model peak browser) and doesn't compete with the time-ordered cross-PID history view. | RunStore is the long-term ledger; History is the short-term replay. Mixing them would re-shape RunStore records and force RunStore writes to carry trajectory data (the ~50 KB-per-record bloat the dispatch flags). Two distinct schemas: lean disk record + rich in-memory record. |

## Q3 in detail — the dead-process retention tension

This is the question that most shapes the data model. The dispatch enumerated three options; here's why option C wins.

### Option A — wall-clock eviction (rejected)

`History.trajectories: HashMap<PID, VecDeque<Sample>>` with eviction by sample age (`sample.timestamp < now - max_age`). When a process dies, its samples slowly drift past the cutoff and the entry shrinks until empty. Then the HashMap key drops.

Why rejected: a process that died 35 minutes ago has zero trajectory data despite (a) the operator wanting to view it on a post-mortem card that just popped up and (b) plenty of memory still being available. The eviction rule conflicts with the actual access pattern.

### Option B — per-PID sample cap + separate "recently-dead" set (rejected)

`History.trajectories` holds live PIDs (sample-count bounded). On exit, MOVE the entry to `History.recently_dead: BTreeMap<DateTime, (PID, Trajectory)>`, evict by death-time when that map exceeds `max_dead_history`.

Why rejected: two separate retention machineries, two lock paths, two eviction rules to reason about. The post-mortem card already has a single source of truth for "what we know about this dead PID" (`LifecycleSummary` in `state.completed`); duplicating that with a sibling structure is needless surface.

### Option C — trajectory rides on `LifecycleSummary` after exit (chosen)

```rust
// in lifecycle/mod.rs
pub struct LifecycleSummary {
    // ... existing fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory: Option<Trajectory>,  // NEW in P5
}

// in history/mod.rs (new)
pub struct Trajectory {
    /// Newest-first samples; capped at `runtime.history_trajectory_samples_per_pid`.
    pub samples: Vec<Sample>,  // Vec, not VecDeque — frozen at exit time
    pub first_sample_at: DateTime<Utc>,
    pub last_sample_at: DateTime<Utc>,
}

pub struct Sample {
    pub timestamp: DateTime<Utc>,    // 12 bytes (i64 + i32)
    pub cpu_pct: f32,                // 4
    pub rss_mb: u32,                 // 4
    pub vram_mb: Option<u32>,        // 8 (4-byte tag + 4-byte payload, aligned)
}                                    // ≈ 28-32 bytes depending on alignment
```

On PID exit (existing site at [runtime.rs:1098](../src/runtime.rs#L1098), inside the `for summary in &lifecycle.recent_exits` loop):

1. Drain `History.trajectories.remove(&pid)`.
2. Move the samples into `summary.trajectory`.
3. Push `summary` into `state.completed` as today (peak-only + trajectory).

Why this wins:
* Lifecycle is the natural retention unit. The existing `completed_history = 50` cap already evicts old dead processes; the trajectory rides along.
* Post-mortem shape-B reads `state.completed[i].trajectory` — same path as the existing peaks/exit-attribution.
* No separate dead-set HashMap; no race on "which structure owns the trajectory for a just-dead PID."
* `RunStore` records stay lean (the new field uses `skip_serializing_if = "Option::is_none"` AND the runtime explicitly sets `record.summary.trajectory = None` BEFORE `rs.append(record)` so disk records are unchanged). The session-lost-on-restart constraint is honored by NOT writing trajectory to disk.

The trade-off documented honestly: the LAST 50 dead processes (in `state.completed`) have full trajectories; processes evicted past that drop to peaks-only (still queryable via RunStore on disk). For longer windows raise `completed_history`; the bound is operator-tunable.

## Memory math (the dispatch wants actual numbers)

### Trajectory store

* Sample size: ~32 bytes (timestamp 12 + cpu f32 4 + rss u32 4 + vram Option<u32> 8 + alignment slop). Conservative.
* Default cap: 1800 samples/PID (30 min @ 1 Hz).
* Per-PID cost: 1800 × 32 = **57.6 KB**.
* Live PID worst case (32 AI workloads — 4× headroom over the operator's 8-process hot path): 32 × 57.6 = **1.84 MB**.
* Plus the `HashMap<PID, ...>` overhead (~48 bytes per entry × 32 = 1.5 KB, negligible).

### Trajectory carried on dead `LifecycleSummary`s

* Same per-trajectory size: ~57 KB worst case (a long-running process that hit the cap before dying).
* `completed_history` cap: 50 entries.
* Worst case all-50 are long-runners: 50 × 57 = **2.85 MB**.
* Realistic: most processes are short-lived and have far fewer samples; expected steady-state is well under 1 MB.

### Event archive

* Per-event size (HistoryEvent — a flat tagged union of exit/kill/regression with the rendered text): ~300 bytes (matches the wire entry shape; cf. [wire.rs:580](../src/web/wire.rs#L580) WireActivityEntry).
* Default cap: 500 entries.
* Steady-state cost: **150 KB**.

### Grand total worst case

Trajectories (live) + Trajectories (dead in completed) + Event archive ≈ **1.84 + 2.85 + 0.15 = 4.84 MB**.

This is rounding error on a binary that already holds NVML buffers, sysinfo handles, and the audit history. Set against the operator's primary win — replayable trajectories during a live session — the cost is uncontroversial.

## Build sequence (8 steps, bisectable, gated by ratification at step 5)

Mirrors the D59-style step decomposition from `PHASE4_AUTOKILL_DESIGN.md`. Steps 1-4 are **observe-only** (no new wire, no new UI); steps 5+ light up consumers.

| Step | Work | Touches | Consumer live? |
|---|---|---|---|
| **0 (PREREQ)** | Add `runtime.history_trajectory_samples_per_pid` and `runtime.history_event_archive_cap` config fields (defaults 1800 and 500). Validates in `Config::validate`. **Pure config plumbing; no readers yet.** No wire change. | `config.rs` | no |
| **1** | New `src/history/mod.rs` with `Trajectory`, `Sample`, `History { trajectories, event_archive }` types. **Structures only, no insertion calls.** Pure types + a few constructors. Unit tests on bounds. | `history/mod.rs` (new), `src/lib.rs` (mod add) | no |
| **2** | Add `Trajectory` to `LifecycleSummary` as `Option<Trajectory>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Field exists but is `None` everywhere. Pin `RunStore` round-trip with `Some(traj)` AND `None`; both must work without schema break. | `lifecycle/mod.rs`, `storage/run_store.rs` (tests) | no |
| **3** | Wire trajectory CAPTURE into `Runtime::tick()`: each per-PID `record_sample` call ([runtime.rs:981-993](../src/runtime.rs#L981)) ALSO pushes a `Sample` into `History.trajectories[pid]`. Bounded ring (sample-count cap from step 0). Drop the per-PID entry when the PID is cleared at the exit drain. | `runtime.rs` | no (capture only) |
| **4** | Wire trajectory HAND-OFF on exit: at the `for summary in &lifecycle.recent_exits` loop ([runtime.rs:1098](../src/runtime.rs#L1098)), drain `History.trajectories.remove(&summary.pid)` and attach to `summary.trajectory` BEFORE the `state.completed.push_back(summary)`. Also set `record.summary.trajectory = None` before the `rs.append(record)` call so RunStore stays peak-only. Pin both invariants in tests. | `runtime.rs` | no (data on `state.completed` only; no surface reads it yet) |
| **5** | Add the EVENT ARCHIVE side. At the existing exit-drain, manual-kill, auto-kill, and regression-record sites push a derived `HistoryEvent` into `History.event_archive`. Existing `state.completed`/`audit`/`regressions` rings unchanged; archive is ADDITIVE. Pin: archive cap respected; events derived from same sources (no new event class). | `runtime.rs`, `history/mod.rs` | no (no consumer yet) |
| **6** | **/api/history endpoint (web, D85 auth-gated)**: GET returns a snapshot of `History` (trajectories + event archive). Wire types: `WireHistorySnapshot`, `WireTrajectory`, `WireHistoryEvent`. Snapshot-on-open semantics — the response is a point-in-time clone. Tests cover the standard 401/200 D85 shape. **CONTRACT BUMP NEEDED** — see flag at the bottom. | `web/history.rs` (new), `web/mod.rs` route, `web/wire.rs` types | yes (machine consumer; no UI yet) |
| **7** | **Web history view**: a Svelte page or collapsible section that fetches `/api/history` on open, renders trajectory curves (per-PID multi-series chart) and an event timeline (paginated). Reload button. Uses the D85 `fetchWithAuth` helper. No live polling — snapshot-on-open per Q5. | `web/src/components/HistoryPage.svelte` (new), `App.svelte`, `web/src/lib/rest.ts` | YES (first operator consumer) |
| **8** | **Post-mortem shape-B**: extend the existing `PostMortemCard` to render the trajectory curves (when `summary.trajectory.is_some()`). This is the SECOND consumer of History. No new endpoint — reads off `state.completed` like the rest of the post-mortem path. | `web/src/components/PostMortem.svelte`, `web/wire.rs` (extend WireExitDetail) | YES |
| **9** (follow-up) | **TUI surface**: a basic timeline browser bound to `H` (capital, to disambiguate from the existing `h` per-model overlay). Browse mode similar to D76 activity browse. Optional; the operator may decide TUI parity isn't worth the chart-rendering complexity in ratatui. | `ui/panels/history.rs` (new), `ui/app.rs`, `ui/mod.rs` | YES (operator-facing) |

Step 6 is the ratification line — once the wire type ships, future changes are contract-affecting. Steps 0-5 are all internal and bisectable without operator-visible behavior change.

## Wire / contract impact (flagged)

Step 6 introduces new wire types: `WireHistorySnapshot`, `WireTrajectory`, `WireHistoryEvent`. Per CLAUDE.md's "**No UX changes without a contract amendment**" rule, Agent A must ship these in `ux_contract` (likely a v0.3.20 minor bump):

* `ux_contract::history::WireHistorySnapshot` — top-level GET /api/history response.
* `ux_contract::history::WireTrajectory` — per-PID samples.
* `ux_contract::history::WireHistoryEvent` — discriminator + fields, mirroring `WireActivityEntry` shape.
* No new strings if the view reuses existing TUI/web copy ("History", "Reload" — the latter is generic enough to land as `ux_contract::history::HISTORY_RELOAD`).

**This is a Contract Amendment Request** filed at design-doc-sign-off time. The consumer (this repo) cannot implement step 6 without the contract types landing first.

## Q-by-Q summary table (for the report-back)

| Q | Resolution one-liner |
|---|---|
| **Q1** | ONE History module containing TWO structures (per-PID trajectories HashMap + cross-PID event archive VecDeque). |
| **Q2** | Per-PID 1800-sample ring (30 min @ 1 Hz). ~32 B/sample × 32 worst-case PIDs = 1.84 MB. |
| **Q3** | On exit, MOVE the trajectory into `LifecycleSummary.trajectory: Option<Trajectory>`. Dead-PID retention rides the existing `completed_history = 50` cap; no separate dead-set machinery. Disk RunStore stays peak-only. |
| **Q4** | Additive `History.event_archive: VecDeque<HistoryEvent>` cap 500 (~150 KB). Wire activity feed cap (50) unchanged. |
| **Q5** | Snapshot-on-open. Web first (post-mortem precedent + chart-rendering fit); TUI follow-up. |
| **Q6** | History sits alongside RunStore. Post-mortem shape-B is the second consumer (reads `summary.trajectory` from `state.completed`). Existing per-model `h` overlay stays — different surface. |

## Surprises found while reading the code

1. **Trajectories don't exist anywhere today.** [`ResourceStats::record`](../src/lifecycle/mod.rs#L30) drops every individual sample — only aggregates survive. Phase 5 has to manufacture the time series; this is not a "surface existing data" dispatch.
2. **The TUI `h` key is already wired** — it opens `App::open_history` with `Runtime::history(model, n).await` returning RunStore records ([app.rs:568](../src/ui/app.rs#L568), [runtime.rs:843](../src/runtime.rs#L843)). The new history view is a DIFFERENT surface (time-ordered cross-PID, not per-model peaks). Both can coexist with distinct keys (`h` vs `H`); the doc explicitly accepts this rather than collapsing the surfaces.
3. **The wire activity feed is already capped at 50** ([ux_contract limits:202](../../ux_contract/src/lib.rs#L202)). Raising that cap to serve History would bloat every `/api/snapshot` poll. Additive `event_archive` is the right shape because the live wire stays the same.
4. **`state.completed` is already lock-step with `state.recent_exit_attribution`** ([runtime.rs:1105-1108](../src/runtime.rs#L1105)). Adding a third lock-step buffer for trajectories would force three-way pop discipline; carrying the trajectory ON the `LifecycleSummary` (the Q3-C choice) avoids that.
5. **`RunRecord` round-trips through serde** at the [storage layer](../src/storage/run_store.rs#L228); the trajectory must use `#[serde(skip_serializing_if = "Option::is_none")]` so a runtime that doesn't capture (or that explicitly nullifies before disk-append) emits the same wire shape pre-D88 readers know — additive, non-breaking.

---

*Promotes to `docs/PHASE5_DESIGN.md` at operator sign-off. Steps 0-4 are observe-only and can land without a contract bump. Step 6 requires a `ux_contract` amendment landing first.*
