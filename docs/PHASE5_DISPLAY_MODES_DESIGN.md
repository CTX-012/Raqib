# Phase 5 — Web Display Modes Design

> **Status**: **RATIFIED by operator 2026-07-08** — the 5 modes were **inferred** here (not previously spec'd in [`ROADMAP.md`](ROADMAP.md) or [`state/BOARD_AUDIT.md`](state/BOARD_AUDIT.md); those documents named only the *count*, `5 web display modes`, without enumeration). The operator reviewed the inferred set against real usage — kiosk maps to the Orin / lab-wall deploy, focus to single-workload deep-dive, timeline to incident review — and CONFIRMED **Dashboard / Focus / Timeline / Kiosk / History**. **Definition of the mode set is owned by the operator; §1.2 is the ratified spec.**
> **Target commit**: `v1.3.2-26-ge873051` (HEAD at authoring). DISPATCH 99.
> **Position in roadmap**: Phase 5 line item ["5 web display modes"](state/BOARD_AUDIT.md#L32) — the roadmap named the count without defining what the modes are; this doc, ratified above, defines them.
> **Pattern**: mirrors DISPATCH 88's design-doc-first / gated-steps discipline, but this pass **defined the feature** (with operator ratification) rather than planning a known one.
> **Sibling docs**: [`PHASE5_HISTORY_DESIGN.md`](PHASE5_HISTORY_DESIGN.md) (shipped as HistoryPage in D95 — one of the modes below reuses it).

---

## 0. Grounding — what already exists

Before proposing modes, this section maps what the dashboard currently is and what data is available, so the proposal answers "against the actual dashboard" (dispatch HARD RULE 2), not against an imaginary one.

### 0.1 Current dashboard shape

[`web/src/App.svelte:29-93`](../web/src/App.svelte#L29-L93) — a single-page grid:

| Region | File | Lines | What |
|---|---|---|---|
| Header | App.svelte:30-49 | 20 | MissionLine + ConnectionPill + theme (dark/light/hc) buttons |
| Alerts region | App.svelte:55-60 | 6 | [`AlertsPanel.svelte`](../web/src/components/AlertsPanel.svelte) — self-hides when empty (103 LoC) |
| Main grid | App.svelte:62-70 | 9 | `grid-cols-1 lg:grid-cols-3` — [`VitalsPanel`](../web/src/components/VitalsPanel.svelte) (col-1) + [`WorkloadsPanel`](../web/src/components/WorkloadsPanel.svelte) + [`ActivityFeed`](../web/src/components/ActivityFeed.svelte) (col-2 stacked) |
| History (collapsible) | App.svelte:77-79 | 3 | [`HistoryPage.svelte`](../web/src/components/HistoryPage.svelte) — D95, snapshot-on-open, own state, 454 LoC |
| Settings (collapsible) | App.svelte:85-87 | 3 | [`SettingsPanel.svelte`](../web/src/components/SettingsPanel.svelte) — D86, tunables + auth, 322 LoC |
| Footer | App.svelte:89-92 | 4 | Tick counter + workload count |

**Panel prop contracts** (relevant to reuse):
- `VitalsPanel(vitals: WireVitals)` — presentational, no own state
- `WorkloadsPanel(workloads: WireWorkload[])` — presentational, delegates to `WorkloadRow`
- `ActivityFeed(activity: WireActivityEntry[])` — presentational (295 LoC — the biggest presentational)
- `AlertsPanel(alerts, recommendations)` — presentational
- `HistoryPage()` — **stateful** (own `expanded`, `snapshot`, `selectedKey` — no props)
- `SettingsPanel()` — **stateful** (own `expanded`, form state, save state — no props)

**Line count total across `web/src/components/`**: 1828 LoC.

### 0.2 Data available

REST routes at [`src/web/mod.rs:135-174`](../src/web/mod.rs#L135-L174):

| Endpoint | Cadence | Shape |
|---|---|---|
| `GET /api/snapshot` | 1 Hz poll (D68 REST-polling) | `WireSnapshot` — vitals + workloads + activity + alerts + recommendations |
| `GET /api/history` | snapshot-on-open (D94) | `WireHistorySnapshot` — event archive + dead-PID index |
| `GET /api/history/trajectory/{pid}` | on-demand (D94) | `WireTrajectory` — per-**dead**-PID sample sequence |
| `GET /api/health` | infrequent | liveness |
| `GET /api/stream` | legacy WS (superseded by REST poll in D68) | full snapshot on each tick |
| `POST /api/settings/*` | user action | tunables (D86) |
| `GET /api/tunables` | on Settings open | current tunables snapshot |

**No endpoint exposes live-PID historical trajectory** — `/api/history/trajectory/{pid}` is designed for dead PIDs. This constraint matters for the FOCUS mode below (Q5).

### 0.3 Render-gate discipline (D87 + D98)

The wire-side gate at [`tests/render_adversarial.rs:1-60`](../tests/render_adversarial.rs#L1-L60) pins the composite-key invariants the Svelte `{#each}` blocks depend on. The audit-of-record at [`tests/empirical/each_key_audit_2026-06-08/REPORT.md`](../tests/empirical/each_key_audit_2026-06-08/REPORT.md) enumerates every `{#each}` in `web/src/` and its key expression. Fixtures live at [`tests/fixtures/render_adversarial/`](../tests/fixtures/render_adversarial/) — 4 adversarial + 1 negative control.

Any mode this proposal adds MUST:
- keep every new `{#each}` on a unique composite key (see per-mode analysis below)
- ship a matching fixture in `tests/fixtures/render_adversarial/`
- once the D98 headless-browser gate lands, mount under the mode's URL param and assert no `each_key_duplicate`

### 0.4 UI-state persistence pattern (that already exists)

- **Theme** — Svelte writable store, no persistence across page reload ([`web/src/lib/stores.ts`](../web/src/lib/stores.ts))
- **Auth token** — `sessionStorage` at [`web/src/lib/rest.ts:83-89`](../web/src/lib/rest.ts#L83-L89), with the intentional note: *"no `localStorage` — survives reloads within the tab but is dropped when the tab closes."*
- **HistoryPage `expanded` / snapshot** — in-memory only, resets on reload
- **SettingsPanel `expanded` / form state** — in-memory only, resets on reload

**Precedent: session-scoped or URL-scoped state only; no long-term localStorage.** The mode-selection persistence proposal below honors this.

### 0.5 Prior intent — none

`grep -rn "display.mode\|DisplayMode\|kiosk\|focus mode" docs/ web/src/` returns exactly three hits, all in [`BOARD_AUDIT.md`](state/BOARD_AUDIT.md), all restating the roadmap phrase "5 web display modes." **No prior document defines what the modes are.** No half-scaffolded mode system exists in the code. This proposal is greenfield within a shipped dashboard.

---

## 1. Q1 — WHAT ARE THE MODES?

### 1.1 The question the tool answers, per mode

The tool is a **model-aware resource monitor for AI workloads on a shared GPU box** (per [`CLAUDE.md`](../CLAUDE.md) "What this project is"). Deployment targets: (a) operator's dev laptop watching localhost, (b) Jetson AGX Orin production, (c) a shared-lab machine with a wall monitor. Each mode should answer **one distinct operator question**:

| Mode | Operator question | Recency | Density | Real deployment |
|---|---|---|---|---|
| **DASHBOARD** | "What's the whole system doing right now?" | live 1 Hz | high grid | dev laptop, default |
| **FOCUS** | "What's this ONE workload doing under load, live?" | live 1 Hz + client-buffered sparklines | high, one-PID | dev laptop while loading a big model |
| **TIMELINE** | "What happened when, in what order?" | live 1 Hz | medium, chronology-first | incident review, active investigation |
| **KIOSK** | "Is anything on fire right now? (glance)" | live 1 Hz | low, glance-only | Orin deployment, lab wall monitor |
| **HISTORY** | "What happened before I got here?" | snapshot-on-open | high, event archive | post-hoc, after an incident |

**Rationale — why 5, not 3 or 6, not 7:**

- Fewer than 5 misses a real question. Merging TIMELINE into DASHBOARD hides the "incident review" case, and TIMELINE + KIOSK are genuinely different: kiosk is *no-interaction*, timeline is *interaction-first*. Merging HISTORY into TIMELINE conflates "live 1 Hz" with "snapshot-on-open" — the D95 lesson (snapshot-on-open, not a live poll) says these are different mental models.
- More than 5 forces the fake ones. A "REPORT" mode (printable snapshot) is a print-stylesheet on any mode, not a mode. A "COMPACT" mode for phones is what Tailwind `lg:` breakpoints already handle. A "DEBUG/JSON" mode is a browser tab open to `/api/snapshot` — no code needed. A "GRAFANA" mode is deliberately out of scope (removed in Sprint 5 per `CLAUDE.md`).
- **5 modes = 5 real operator questions.** Not 5 forced boxes.

### 1.2 Per-mode specification

#### DASHBOARD (default)
- **What it shows**: exactly the current App.svelte grid — Vitals card-1 + Workloads/Activity col-2, alerts region above, footer with tick counter.
- **Who / when**: interactive operator on dev laptop; the "opened localhost:7070 to see what's going on" default.
- **Layout**: unchanged from today.
- **New components**: none. This mode is `App.svelte`'s current body verbatim (with the collapsible HistoryPage + SettingsPanel moved out — they become mode-agnostic access controls in the header, or dedicated modes).
- **Data**: `/api/snapshot` at 1 Hz (existing).
- **Keyed lists**: all existing (WorkloadsPanel `w.pid`, ActivityFeed composite, AlertsPanel composite, VitalsPanel thermals).

#### FOCUS
- **What it shows**: one selected workload dominating the screen. Its live vitals (CPU%, RAM, VRAM, tok/s, fps, KV%, activity state) as prominent numbers + client-side sparklines (60-tick / 1-minute rolling buffer of recent snapshots for THIS PID). A side rail lists other workloads (compact WorkloadRow — click to switch focus). Alerts/recommendations still surface above.
- **Who / when**: operator about to load a big model, benchmark a specific inference server, or debug a specific PID's memory growth.
- **Layout**:
  ```
  ┌─────────────────────────────┬───────────────┐
  │ FOCUSED WORKLOAD            │  side rail    │
  │ ollama · phi3-mini (PID 42) │  · claude 47  │
  │ CPU  ▁▂▂▃▅▅▄▃▃▂▁            │  · ros2  93   │
  │ RAM  1.2 GB   [====    ]    │  · yolo 118   │
  │ VRAM 8.4 GB   [======  ]    │               │
  │ tok/s  42.1                 │               │
  │ recent trajectory (60s)     │               │
  └─────────────────────────────┴───────────────┘
  ```
- **New components**: `FocusView.svelte` (~250 LoC estimate — the big-number widgets + sparklines + PID picker).
- **Reuse**: `WorkloadRow.svelte` for the side rail (compact variant). `VitalsPanel`'s severity classification helper (imports, not the component). Client-side sparkline logic is new — DO NOT reuse `TrajectoryChart.svelte` verbatim (that's for dead-PID trajectories from `/api/history/trajectory/{pid}`).
- **Data**: `/api/snapshot` at 1 Hz (existing). Sparklines accumulated **client-side** from the 1 Hz polls (a rolling 60-entry array per metric per PID). **No new endpoint** in the first cut. §5 notes an optional /api/live/trajectory/{pid} v-next.
- **Keyed lists**: side-rail workload list (`w.pid`, same as WorkloadsPanel). Sparkline points render with `{#each}` — key on `(metric, index)` composite. Adds one new fixture: `F5_focus_sparkline_dense.json`.

#### TIMELINE
- **What it shows**: alerts + activity feed dominate the viewport (~2/3 of screen). Vitals shrinks to a single-row strip along the top (compact meters). Workloads shrink to a compact side rail. Chronology-first: events sorted newest-first, timestamps prominent.
- **Who / when**: incident review — operator investigating "something died at 11:47, what happened?" or actively watching an alert cascade.
- **Layout**:
  ```
  ┌────────────────────────────────────────────────┐
  │ RAM 68% · VRAM 91% · CPU 45% · Thermal ⚠ 87°C  │  ← 1-line vitals
  ├──────────────────────────────┬─────────────────┤
  │ ALERTS                       │  workloads      │
  │ 11:52 VRAM 92% ollama (kill?)│  · ollama 42    │
  │ 11:47 RAM 88%  system         │  · claude 47   │
  │                              │  · ros2  93     │
  │ ACTIVITY                     │                 │
  │ 11:52 kill ollama SIGTERM    │                 │
  │ 11:52 exit ollama governor   │                 │
  │ 11:41 exit yolo unknown      │                 │
  └──────────────────────────────┴─────────────────┘
  ```
- **New components**: `TimelineView.svelte` wrapper (~120 LoC estimate — layout, no new logic). Uses existing AlertsPanel + ActivityFeed inline. Uses `VitalsPanel` in a "strip" prop mode OR a new `VitalsStrip.svelte` (~80 LoC) — the design LEAN is a new small `VitalsStrip.svelte` because forcing VitalsPanel into two shapes via a prop grows the biggest presentational panel further; a dedicated strip stays presentational and small.
- **Reuse**: AlertsPanel + ActivityFeed as-is. WorkloadRow for side rail. New: `VitalsStrip.svelte`.
- **Data**: `/api/snapshot` at 1 Hz (existing).
- **Keyed lists**: reuses existing. No new fixtures beyond the "many events at similar timestamps" edge case — `F6_timeline_dense_ordering.json`.

#### KIOSK
- **What it shows**: 5-6 big numbers on the screen. No interaction. RAM %, VRAM %, workload count, degraded count, current thermal max, current alert count. Big font (readable from across a room), high contrast (auto-selects `hc` theme by default when this mode is loaded), color goes red on any critical.
- **Who / when**: passive display on a wall monitor, a second screen next to the operator's main workstation, or an Orin deployment showing "the box is up." No mouse expected.
- **Layout**:
  ```
  ┌──────────────────────────────────────────────┐
  │  RAM     68%     VRAM     91%   [CRITICAL]  │
  │                                              │
  │  Workloads     7    Degraded    1            │
  │                                              │
  │  Thermal   87°C   Alerts   2  active         │
  └──────────────────────────────────────────────┘
  ```
- **New components**: `KioskView.svelte` (~180 LoC estimate — big-number widgets + severity classification + auto-refresh polling).
- **Reuse**: severity classification from VitalsPanel's helpers (imported functions, not the component). NO reuse of the existing panels themselves — they're too dense for kiosk font sizes.
- **Data**: `/api/snapshot` at 1 Hz (existing). Optionally reduce cadence (e.g. 5 Hz) to reduce network chatter on Orin deployments — but simpler to keep at 1 Hz and add cadence as a URL param if it matters.
- **Keyed lists**: none new (no `{#each}` over lists — everything is a fixed slot).

#### HISTORY
- **What it shows**: [`HistoryPage.svelte`](../web/src/components/HistoryPage.svelte) promoted from a collapsible to a full-viewport mode. Event archive + dead-PID index + on-demand trajectory chart. Live 1 Hz suspended (nothing else on screen to reflect it).
- **Who / when**: post-hoc analysis after an incident, before writing a report, or "what did I miss over lunch."
- **Layout**: the existing HistoryPage layout, but wider and without the collapsible chrome. Header keeps mode switch + refresh.
- **New components**: none. HistoryPage already exists at 454 LoC and is complete.
- **Reuse**: HistoryPage.svelte verbatim, minus the collapsible outer `{#if expanded}` block. Extract that boilerplate into `App.svelte`'s mode-routing, leaving HistoryPage as pure content.
- **Data**: `/api/history` snapshot-on-open + `/api/history/trajectory/{pid}` on drill-in (existing).
- **Keyed lists**: existing D95 composites (`${kind}-${pid}-${timestamp}` for events, `${pid}-${exit_time}` for dead PIDs).

### 1.3 What's OUT of scope in mode set

- **Grafana-style timeline editor**: Sprint 5 removal (`CLAUDE.md`). Not a mode.
- **Multi-monitor / detached windows**: browser-native (`window.open`) is enough for wall-monitor use.
- **Report/print mode**: a `@media print` stylesheet on any mode covers this. Not a distinct mode.
- **Mobile-first / phone-optimized layout**: Tailwind `lg:` breakpoints already collapse the grid; not a distinct mode.
- **Debug/raw-JSON mode**: `curl /api/snapshot | jq` and browser DevTools already serve this.
- **Server-selected mode based on user agent**: the operator picks; the server doesn't decide.

---

## 2. Q2 — HOW DO MODES SWITCH?

### 2.1 Mechanism: URL query parameter + header dropdown

**URL scheme**: `?mode=<mode>` — e.g. `localhost:7070/?mode=focus&pid=42`, `localhost:7070/?mode=kiosk`.

Valid values: `dashboard` (default when omitted), `focus`, `timeline`, `kiosk`, `history`.

**Header UI**: a small `<select>` next to the theme buttons (same visual weight as theme). Selecting changes the URL query param (`history.pushState`) so:
- The page URL reflects the current mode (bookmarkable, shareable, refresh-safe).
- Back/forward navigate through mode history.
- The wall-monitor operator bookmarks `?mode=kiosk` and reload always brings them back to kiosk.

### 2.2 Why URL, not sessionStorage or heavy router

- **URL is shareable + refresh-safe**. Wall-monitor use case: operator bookmarks `localhost:7070/?mode=kiosk`. Every reload gives them kiosk. `sessionStorage` doesn't survive reload of a bookmarked URL; `localStorage` violates the [D68 no-localStorage precedent](../web/src/lib/rest.ts#L84).
- **URL is one query param, not a router**. No `svelte-spa-router` dep, no `#/routes/foo/bar` hash paths. `?mode=X` is 30 lines of Svelte reactive `$: mode = new URLSearchParams(window.location.search).get('mode') ?? 'dashboard'`. Zero new dependencies.
- **FOCUS gets a second param cleanly**: `?mode=focus&pid=42` — deep-linkable "watch this PID."

### 2.3 Reactive plumbing

Wire the mode selection into a Svelte store (`web/src/lib/stores.ts` gets a `mode: Writable<Mode>` alongside `theme`). Two-way sync with URL:
- Component subscribes to `mode` → routes to `<DashboardView>` / `<FocusView>` / etc.
- Selecting a new mode in the header → `mode.set(x)` → `history.pushState({}, '', `?mode=${x}` + preserve other params)`.
- Browser back/forward → `popstate` handler → re-parses URL → `mode.set(parsed)`.

### 2.4 What happens on invalid mode

`?mode=nonsense` → fall back to `dashboard`. Don't 400, don't blank the page, don't confuse the operator with an error toast. This matches the theme handling at [`App.svelte:24-26`](../web/src/App.svelte#L24-L26) which similarly tolerates arbitrary theme names by falling to `dark`.

---

## 3. Q3 — SHARED vs PER-MODE (reuse map)

| Component | DASHBOARD | FOCUS | TIMELINE | KIOSK | HISTORY | Cost |
|---|---|---|---|---|---|---|
| `MissionLine` | ✅ header | ✅ header | ✅ header | ❌ (kiosk has own big banner) | ✅ header | reuse-only |
| `ConnectionPill` | ✅ header | ✅ header | ✅ header | ✅ header | ✅ header | reuse-only |
| `AlertsPanel` | ✅ region | ✅ region | ✅ **dominant** | ❌ (kiosk has own count) | ❌ | reuse-only |
| `VitalsPanel` | ✅ col-1 | ❌ (uses helpers) | ❌ (uses VitalsStrip) | ❌ (uses helpers) | ❌ | reuse-only |
| `VitalsStrip` (new) | ❌ | ❌ | ✅ top | ❌ | ❌ | **~80 LoC new** |
| `WorkloadsPanel` | ✅ | ❌ (uses WorkloadRow only) | ❌ (uses WorkloadRow only) | ❌ | ❌ | reuse-only |
| `WorkloadRow` | (via panel) | ✅ side rail | ✅ side rail | ❌ | ❌ | reuse-only |
| `ActivityFeed` | ✅ | ❌ | ✅ **dominant** | ❌ | ❌ | reuse-only |
| `RecommendationCard` | (via AlertsPanel) | (via AlertsPanel) | (via AlertsPanel) | ❌ | ❌ | reuse-only |
| `HistoryPage` | (collapsible today) | ❌ | ❌ | ❌ | ✅ **dominant** | reuse (drop collapsible chrome) |
| `TrajectoryChart` | ❌ | ❌ | ❌ | ❌ | ✅ (dead-PID drill-in, via HistoryPage) | reuse-only |
| `SettingsPanel` | (collapsible today) | ❌ | ❌ | ❌ | ❌ | reuse (retire from collapsible; access via a header cog OR promote to a modal) |
| `DashboardView` (new wrapper) | ✅ | ❌ | ❌ | ❌ | ❌ | ~30 LoC new (extracts App's main-grid block) |
| `FocusView` (new) | ❌ | ✅ | ❌ | ❌ | ❌ | **~250 LoC new** |
| `TimelineView` (new) | ❌ | ❌ | ✅ | ❌ | ❌ | **~120 LoC new** |
| `KioskView` (new) | ❌ | ❌ | ❌ | ✅ | ❌ | **~180 LoC new** |
| `HistoryView` (thin wrapper) | ❌ | ❌ | ❌ | ❌ | ✅ | ~30 LoC (drops HistoryPage's collapsible outer) |

**New-code total: ~690 LoC of new components + fixtures + tests**. Existing 1828 LoC of components: **fully reused** (none rewritten, none forked).

### 3.1 What settings does with modes

The current collapsible `SettingsPanel` at `App.svelte:86` presents a design choice this proposal doesn't fully answer:

- **Option A**: keep as a collapsible in `DASHBOARD` mode only. Other modes don't show it. Operator switches to Dashboard to change settings.
- **Option B**: promote to a modal available from the header cog icon in all modes. Consistent access.
- **Option C**: make Settings a 6th mode. **Rejected** — settings isn't a "way to look at the data," it's a way to change the tunables. Different concern.

**Inspector lean: Option B** (header cog / modal). Consistent, no mode blocks access to tunables. Modal instead of collapsible aligns with the "mode dominates the viewport" pattern the other modes establish. But this is an incidental UX call — the proposal ships mode routing first (§4) and can carry SettingsPanel as-is in Dashboard while the modal is a follow-up.

---

## 4. Q4 — STATE / persistence

### 4.1 Mode selection

- **Persistence layer**: URL query param (`?mode=X`).
- **Rationale**: shareable, refresh-safe, no localStorage drift. Matches the D68 no-localStorage precedent.
- **Cross-session**: bookmarking preserves it. Fresh tab defaults to Dashboard.
- **Cross-reload**: preserved (URL survives reload).
- **Cross-browser-close-and-open of bookmark**: preserved.

### 4.2 Per-mode state

| Mode | State-owning | Where |
|---|---|---|
| DASHBOARD | none (fully derived from `$snapshot` store) | — |
| FOCUS | selected PID + client-side sparkline buffers | URL: `?mode=focus&pid=42`. Sparklines: in-memory rolling arrays (reset on reload — acceptable per the "look at this workload NOW" mental model) |
| TIMELINE | filter chips (e.g. "show only kill events") | if we add them: URL query param `&filter=kill,exit` |
| KIOSK | no user state (glance-only) | — |
| HISTORY | selected dead-PID for trajectory drill-in | in-memory only (matches D95's current behavior) |

### 4.3 Theme × mode interaction

Themes (`dark`/`light`/`hc`) are **orthogonal** to modes. Any mode renders in any theme.

**One default override**: KIOSK mode defaults to `hc` (high-contrast) when the URL loads *without* an explicit theme selection. Rationale: kiosk is often on a distant screen; high-contrast is the safer glance-default. Operator can still switch away from hc while in kiosk mode.

Implementation: when `?mode=kiosk` is set AND no prior theme was selected (fresh session), initialize theme to `hc` before first render. Do NOT force `hc` on every kiosk mount — respect operator's later choice.

---

## 5. Q5 — DATA COST (per mode)

| Mode | Endpoints | New endpoints? | Contract bump? |
|---|---|---|---|
| DASHBOARD | `/api/snapshot` (1 Hz) | — | — |
| FOCUS | `/api/snapshot` (1 Hz) + client-buffered sparklines | **flag** (see §5.1) | — |
| TIMELINE | `/api/snapshot` (1 Hz) | — | — |
| KIOSK | `/api/snapshot` (1 Hz) | — | — |
| HISTORY | `/api/history` (snapshot-on-open) + `/api/history/trajectory/{pid}` (on-demand) | — | — |

**No new endpoints needed for the initial ship.** No `ux_contract` bump needed. Every mode renders from the existing wire.

### 5.1 FOCUS mode — the one caveat

Today's `/api/history/trajectory/{pid}` is designed for **dead** PIDs — the tick loop populates `dead_pid_trajectories` at process exit. **Live PIDs don't have a persistent trajectory endpoint.**

FOCUS mode's sparklines can be built two ways:
- **(A) Client-buffered** (recommended for step 1): each 1 Hz poll appends the focused PID's current metrics to a rolling 60-entry client-side array. On reload the sparklines start empty and fill over the next 60 seconds. Zero backend work.
- **(B) New endpoint `/api/live/trajectory/{pid}`** (v-next, optional): tick loop maintains a rolling ring for live PIDs too. First-load populates immediately.

**Recommendation**: ship (A) first. It's honest (no new endpoint, no contract concern, no schema drift), it satisfies the "watch this workload NOW" use case, and if operators ask for cross-reload persistence, (B) lands as a targeted follow-up dispatch with its own CAR and gate.

If (B) ever ships, it needs a Contract Amendment Request through Agent A (new type `WireLiveTrajectory` in ux_contract), a new `SharedLiveTrajectoryView`-style cross-thread cell like `SharedHistoryView` at [`src/web/history.rs:184`](../src/web/history.rs#L184), a fixture in `tests/fixtures/render_adversarial/`, and a test. **~150 LoC of backend + ~40 LoC of frontend.** Explicitly flagged as a **v1.4.x candidate**, not part of this Phase-5 arc.

### 5.2 Kiosk cadence — noted, not scoped

Kiosk on Orin over a corporate LAN wants low network chatter. Two options:
- Reduce poll cadence to 5 Hz (or 10 Hz) via a URL param `?mode=kiosk&interval=5s`.
- Keep 1 Hz.

Not decided in this proposal. Recommend keeping 1 Hz first cut; add a URL param if operator reports network chatter.

---

## 6. Q6 — RENDER-GATE FIT

### 6.1 New `{#each}` blocks by mode

| Mode | New keyed lists | Composite key | Existing gate covers? |
|---|---|---|---|
| DASHBOARD | none (identical to today) | — | ✅ D87 |
| FOCUS | (1) side-rail workload list; (2) sparkline points per metric | (1) `w.pid` (already covered); (2) `${metric}-${index}` | (1) ✅; (2) **new** |
| TIMELINE | none (reuses AlertsPanel/ActivityFeed lists) | — | ✅ D87 |
| KIOSK | none (fixed slots, no `{#each}` over lists) | — | ✅ D87 |
| HISTORY | none new (reuses D95 keyed lists) | — | ✅ D87 |

Only FOCUS introduces genuinely new keyed lists — and only one (sparkline points) is new. The composite `${metric}-${index}` is index-based within a fixed-length rolling buffer per metric, so collisions are structurally impossible; still, the fixture pins the invariant.

### 6.2 New render-adversarial fixtures

Add to [`tests/fixtures/render_adversarial/`](../tests/fixtures/render_adversarial/):

- **`F5_focus_sparkline_dense.json`** — a live-PID snapshot + a client-side sparkline buffer with 60 entries at collision-risky metric values (all same y, all same y at repeat x). Asserts the `(metric, index)` composite disambiguates.
- **`F6_kiosk_all_criticals.json`** — a snapshot with every meter at 99%, thermal at 95°C, RAM at 98%. Asserts kiosk's big-number widgets don't visually crash or hit each-key issues.
- **`F7_timeline_dense_ordering.json`** — 40+ activity + alert events at similar timestamps. Asserts timeline's density doesn't collide against the D71-composite key.

Each fixture ships alongside a Rust-side wire-gate test in [`tests/render_adversarial.rs`](../tests/render_adversarial.rs) asserting the composite-key invariant against the fixture.

### 6.3 D98 headless-browser gate extension

When D98's Playwright / Puppeteer harness lands, it must:

1. **Mount each mode explicitly** by URL — `?mode=dashboard`, `?mode=focus&pid=42`, `?mode=timeline`, `?mode=kiosk`, `?mode=history` — against every fixture.
2. **Assert no `each_key_duplicate` in console** across all (mode, fixture) pairs (5 modes × 5+ fixtures = 25+ assertions).
3. **Assert visible cards present**:
   - DASHBOARD: VitalsPanel + WorkloadsPanel + ActivityFeed all rendered
   - FOCUS: FocusView renders + PID picker functional
   - TIMELINE: AlertsPanel + ActivityFeed dominate
   - KIOSK: big-number widgets present
   - HISTORY: HistoryPage's events + dead-PID index rendered

The D98 test-matrix expansion is scoped in **Step 7** of the build sequence below.

---

## 7. Build sequence (bisectable, gated)

Nine steps. Each step ships as one PR / dispatch, individually reverting-safe, individually gate-tested. Steps in **bold** ship user-visible features; unbolded steps are scaffolding.

| Step | Work | Touches | User-visible? | Bisect scope |
|---|---|---|---|---|
| **1** | **Mode scaffold** — `mode: Writable<Mode>` store, URL param parser, header dropdown (5 options), reactive routing in App.svelte. All modes route to `<DashboardView>` (which contains today's main-grid body) EXCEPT itself. Kiosk/Focus/Timeline/History show a "coming soon" empty state. Dashboard is byte-identical to today. | `web/src/lib/stores.ts`, `App.svelte`, new `views/DashboardView.svelte` | yes (dropdown appears) | full |
| **2** | **HISTORY mode** — extract HistoryPage from collapsible, wrap as `HistoryView`. `?mode=history` renders it full-viewport. | new `views/HistoryView.svelte`, refactor `App.svelte` | yes | history-only |
| **3** | **KIOSK mode** — implement `KioskView.svelte` (big numbers, severity classification, kiosk auto-hc default). No new endpoints; reads `$snapshot`. Fixture `F6_kiosk_all_criticals.json` + wire-gate test. | `views/KioskView.svelte`, fixture, `tests/render_adversarial.rs` | yes | kiosk-only |
| **4** | **TIMELINE mode** — implement `TimelineView.svelte` + `VitalsStrip.svelte`. Uses AlertsPanel + ActivityFeed as-is. Fixture `F7_timeline_dense_ordering.json` + wire-gate test. | `views/TimelineView.svelte`, `components/VitalsStrip.svelte`, fixture, `tests/render_adversarial.rs` | yes | timeline-only |
| **5** | **FOCUS mode** — implement `FocusView.svelte` with client-buffered sparklines. `?mode=focus&pid=X`. Side rail reuses WorkloadRow. Fixture `F5_focus_sparkline_dense.json` + wire-gate test. | `views/FocusView.svelte`, fixture, `tests/render_adversarial.rs` | yes | focus-only |
| **6** | **Settings access** — decide Option A/B/C per §3.1. If B (header cog + modal), promote SettingsPanel to a modal available in all modes. If A, keep in Dashboard collapsible; annotate other modes. | `App.svelte`, possibly wrap `SettingsPanel.svelte` | operator decision-dependent | settings-only |
| 7 | **D98 gate extension** — Playwright harness mounts each (mode × fixture) pair and asserts no `each_key_duplicate` + expected panel presence. 5 × 5 = 25+ assertions. | new `tests/playwright/*.spec.ts`, CI wiring | test-only | gate |
| 8 | **Docs** — CHANGELOG entries, `docs/PHASE5_DISPLAY_MODES_DESIGN.md` promoted to "shipped" status, `README.md` "web dashboard" section names the modes. | docs, CHANGELOG | doc | doc |
| 9 (optional, v-next) | `/api/live/trajectory/{pid}` for cross-reload sparkline persistence in FOCUS mode. Contract Amendment Request → Agent A → new `WireLiveTrajectory` type. | ux_contract, `src/web/history.rs` (or new `src/web/live_traj.rs`), FocusView, fixture, test | operator-visible on reload | v1.4.x candidate |

Order rationale:
- **Step 1** ships the switch mechanism first, DORMANT (all modes but Dashboard placeholder). Reverting just this step returns the app to today's exact behavior. Bisectable.
- **Step 2 (HISTORY)** ships first-content because HistoryPage already exists — highest value-per-LoC. If Step 2 breaks, only history is affected.
- **Step 3 (KIOSK)** next because it's visually the most different (biggest signal of "modes work") but the smallest new-logic build.
- **Step 4 (TIMELINE)** is layout-only, reuses two existing panels. Small.
- **Step 5 (FOCUS)** is the biggest single-component build. Ships last of the modes so the earlier steps have shaken out the switch mechanism.
- **Step 6 (Settings)** ships after all mode-view builds so the operator can see the mode set before choosing A/B/C.
- **Step 7 (D98)** ships once all modes have fixtures. One-shot gate expansion.

---

## 8. Contract / endpoint change flags

Summary of every hop that touches the wire or the contract:

| Step | Contract change? | New endpoint? | CAR needed? |
|---|---|---|---|
| 1-8 | **none** | **none** | **none** |
| 9 (v-next) | **yes** — new `WireLiveTrajectory` type in `ux_contract` | **yes** — `GET /api/live/trajectory/{pid}` | **yes** — file with Agent A |

Steps 1-8 are pure consumer-side work. **The Phase-5 modes arc can complete without a single contract touch.** That's the honest ship target.

Step 9 is EXPLICITLY out of this Phase-5 scope. If FOCUS mode users report the client-buffered sparkline reset is unacceptable, step 9 opens as a targeted follow-up.

---

## 9. Render-gate extension plan

Restated for operator scanning:

- **Wire gate** (D87): 3 new fixtures (F5/F6/F7) + 3 new test entries in `tests/render_adversarial.rs`. Land in the same step as each new mode (steps 3-5).
- **Browser gate** (D98, when it lands): mount each mode by URL param against every fixture. 25+ assertions. Land in step 7 as a one-shot.
- **Existing composites**: unchanged. `WorkloadsPanel (w.pid)`, `VitalsPanel (label, idx)`, `ActivityFeed (kind, pid, timestamp)`, `AlertsPanel (alert_id, pid || 'system')`, `HistoryPage (kind, pid, timestamp)` — all remain load-bearing across every mode that reuses them.

**No composite key changes required.** The mode arc rides on the existing key discipline; the gate is *extended*, not *redesigned*.

---

## 10. Surprises worth flagging

Things that surfaced while reading the codebase and are worth an operator eyeball:

- **No mode system exists in code, half-scaffolded or otherwise** (confirmed via grep §0.5). Greenfield within a shipped dashboard. Clean slate for step 1.
- **The current `AlertsPanel` region is *outside* the main grid** ([`App.svelte:55-60`](../web/src/App.svelte#L55-L60)). Every mode except KIOSK keeps this convention (alerts above the mode's viewport) — matches the TUI banner behavior noted in the D42 comment. KIOSK folds alert count into a big-number widget instead.
- **The `SettingsPanel` collapsible convention** ([`App.svelte:85-87`](../web/src/App.svelte#L85-L87)) is duplicated with `HistoryPage` — both are "collapse me until you need me" patterns. Once we promote HistoryPage to a full mode, SettingsPanel becomes the last collapsible on Dashboard, which reads inconsistently. This is what motivates §3.1 Option B (modal-ize Settings).
- **`ActivityFeed.svelte` is 295 LoC, the biggest presentational** — it's already load-bearing on DASHBOARD and TIMELINE. Watch for regressions when TIMELINE emphasis reshuffles its visual weight. The D65/D71 keying discipline stays intact.
- **Grafana was deliberately removed** (Sprint 5 per `CLAUDE.md`). Don't propose modes that re-invent that scope (custom dashboard editor, per-panel drag). Modes are curated views, not user-configurable canvases.
- **Windows binary halted** (per `CLAUDE.md`) — the mode set here is Linux-web-first. When Windows resumes, mode parity is Agent C's problem, mirroring the current dashboard-parity approach.
- **`ux_contract` prior authority**: any wire change goes through Agent A. Steps 1-8 avoid this; step 9 (v-next) triggers it and is deliberately scoped out.

---

## 11. Summary card

| Field | Value |
|---|---|
| **Number of modes** | **5** (Dashboard, Focus, Timeline, Kiosk, History) — one per real operator question; argued vs 3 (missing coverage) and vs 6+ (forced boxes). |
| **Switch UX** | URL query param `?mode=X` (+ `&pid=Y` for FOCUS) with a header `<select>` dropdown mirroring the theme buttons. `history.pushState` for shareable + refresh-safe. |
| **Persistence** | URL only. No localStorage (D68 precedent). SessionStorage only for auth token (unchanged). |
| **New LoC (frontend)** | ~690 LoC across 5 new view components + 1 new VitalsStrip. Existing 1828 LoC fully reused. |
| **New endpoints** | **NONE** for steps 1-8. Optional `/api/live/trajectory/{pid}` deferred to v1.4.x candidate (step 9) with a CAR. |
| **Contract bumps** | **NONE** for steps 1-8. Step 9 requires one CAR (v1.4.x candidate). |
| **Fixtures added** | 3 (F5_focus_sparkline_dense, F6_kiosk_all_criticals, F7_timeline_dense_ordering) |
| **Wire-gate tests** | 3 new (one per fixture, in `tests/render_adversarial.rs`) |
| **Browser-gate expansion** | 25+ (5 modes × 5+ fixtures) assertions in step 7's Playwright harness |
| **Bisectable steps** | 8 mandatory + 1 optional v-next |
| **Sequenced ship order** | scaffold → HISTORY → KIOSK → TIMELINE → FOCUS → settings-decision → D98 gate → docs → (v-next endpoint) |
| **Ratified 2026-07-08** | ✅ mode set (5, as enumerated in §1.2), ✅ switch mechanism (URL param + header dropdown per §2.1), ⚠ settings access A/B/C — deferred to step 6 |

---

*Mode set ratified 2026-07-08. Sub-dispatches for steps 1–5 fire against §1.2 as written. Step 6 (settings access A/B/C) is the only remaining operator-decision item — carries into that step's dispatch. If the mode set is later expanded (a "REPORT" mode, a "COMPACT" mode, etc.), that reopens §1.2 in this same file before the affected step re-fires.*
