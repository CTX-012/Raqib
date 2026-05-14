# BACKLOG

Issues / improvements discovered during in-flight rows that don't fit the
current row's scope. Each entry includes the row that surfaced it.

## v1.1 (post-v1.0)

### telemetry: query Triton `/v2/models` to refine WorkloadCategory for hosted models

**Surfaced by:** L11a workload-category dispatch.

Triton Inference Server and TorchServe are general-purpose model
servers — they can host any model type (LLM, Vision, Embeddings).
Without HTTP introspection of the server's model registry, the
classifier currently maps both to `WorkloadCategory::Unknown` (honest
about the ambiguity but suboptimal UX for users running, say, an
LLM under Triton).

Proposal for v1.1: add a Triton telemetry sampler that scrapes
`/v2/models` (KFServing v2 protocol) at startup for each Triton PID,
inspects the listed models' platforms / config metadata, and refines
the workload category. Same shape for TorchServe via its
`/management` endpoint.

Out of scope for v1.0 — the contract's v1.0 surface has no §2 metric
formatter that would change behavior with a refined category for
these servers.

### test deflake: `telemetry::rapl::tests::delta_math_handles_wraparound`

**Surfaced by:** L11a smoke run.

RAPL delta-math test has timing-sensitive assertions and flakes
under load (passes alone, fails ~1 in 50 in the full workspace
suite). Not a regression — pre-existed across L1-L8.

Proposal: tighten the test bounds or use a synthetic clock to
avoid wall-clock dependency. Low priority; not blocking ship.

## Resolved Contract Amendment Requests

### CAR-7 — RESOLVED in v0.3.4

`ux_contract::status::COLD_LOADING = "cold-loading"`. Adopted by
L11c in `src/ui/panels/workloads.rs::primary_metric`; the local
`COLD_LOADING` literal was retired.

### CAR-8 — RESOLVED in v0.3.4

`ux_contract::workload_category::GROUP_HEADER_*` (LLM / VISION /
ROS2 / EMBEDDINGS / UNKNOWN). Contract refined the proposal to
ship the full rule-line strings (`"── LLM ──"` etc.), not just
labels — saves a `format!` at every render. Adopted by L11c via
`panels::workloads::category_header`. The local
`WorkloadCategory::label()` helper was retired; the
`WorkloadCategory` enum itself stays in this tree per the
orchestrator's "KEEP CONST-ONLY for v1.0" decision.

## Pending Contract Amendment Requests

### CAR-11: top-processes panel title + sort indicators

**Surfaced by:** L13 Top processes panel + L14 t-key sort cycle.
UX_CONTRACT.md §1 region 5 illustrates the panel header as
"Top processes (by RAM)" with a parenthesised sort indicator.
v0.3.4 has no `ux_contract::top_processes::*` const for either
the panel title or the per-sort indicator labels.

L13 uses a local literal `"Top processes (by RAM)"`; L14 will
need additional locals for "(by CPU)" and "(by VRAM)" when the
sort cycle ships.

Suggested addition to `ux_contract`:
```rust
pub mod top_processes {
    /// Block title for the §1 region 5 panel.
    pub const PANEL_TITLE_PREFIX: &str = "Top processes";
    /// Sort indicator suffix; concatenated as
    /// `"{PANEL_TITLE_PREFIX} (by {SORT_LABEL})"`.
    pub const SORT_BY_RAM: &str = "RAM";
    pub const SORT_BY_CPU: &str = "CPU";
    pub const SORT_BY_VRAM: &str = "VRAM";
}
```

Also consider routing the §1 region 5 prose-vs-example
contradiction (prose says "Filters … processes already in
Workloads"; example shows AI process `ollama` in Top
processes). L13 implements per the example + the orchestrator's
brief (un-filtered); the prose should be updated to match.

### CAR-9: per-category degraded-line templates

**Surfaced by:** L12 degraded-row expansion. UX_CONTRACT.md §2
locks a per-category schema for the second indented line shown
under Attention/Critical workloads:

| Category | Schema |
|---|---|
| LLM | `KV {pct}% · queue {n} · p99 {ms}ms · baseline {tok/s} · {±delta}%` |
| Vision | `VRAM {pct}% · {phase} · baseline {fps} · {±delta}%` |
| Embeddings | `batch {n} · p99 {ms}ms · baseline {emb/s} · {±delta}%` |
| ROS2 | `topics {n} · queue {n} · baseline {Hz} · {±delta}%` (v1.1+) |
| Loading | `loaded {gb} / {total} GB · {n} disk reads remaining` |
| Unknown | `(unrecognized AI workload — no metrics)` |

These need to ship as `ux_contract::degraded_line::*` const so
both Linux and Windows render the same strings. v1.0's data layer
populates only a subset of the placeholders (VRAM%, KV%, RAM% —
not queue depth, p99, live baseline, or {±delta}%); the contract
strings would still be authoritative once those telemetry
features land.

L12 ships a content-light placeholder rendering — for an
Attention/Critical row, list the breaching metrics inline
(`"VRAM 87% · KV 84%"`) using local format strings. No baseline
or delta tracking yet. Once CAR-9 lands and the contract const
exist, a follow-up adoption row swaps the locals for the const
references; once live telemetry tracks queue/p99/baseline, the
fuller §2 schema can fill in.

Suggested addition to `ux_contract`:
```rust
pub mod degraded_line {
    /// `KV {pct}% · queue {n} · p99 {ms}ms · baseline {tok/s} · {±delta}%`
    pub const LLM: &str = "KV {kv_pct}% · queue {queue} · p99 {p99_ms}ms · baseline {baseline_tps} · {delta_pct}%";
    /// `VRAM {pct}% · {phase} · baseline {fps} · {±delta}%`
    pub const VISION: &str = "VRAM {vram_pct}% · {phase} · baseline {baseline_fps} · {delta_pct}%";
    /// `batch {n} · p99 {ms}ms · baseline {emb/s} · {±delta}%`
    pub const EMBEDDINGS: &str = "batch {batch} · p99 {p99_ms}ms · baseline {baseline_eps} · {delta_pct}%";
    /// `topics {n} · queue {n} · baseline {Hz} · {±delta}%` (v1.1+)
    pub const ROS2: &str = "topics {topics} · queue {queue} · baseline {baseline_hz} · {delta_pct}%";
    /// Cold-start phase progress.
    pub const LOADING: &str = "loaded {loaded_gb} / {total_gb} GB · {disk_reads} disk reads remaining";
    /// AI process without category-specific telemetry.
    pub const UNKNOWN: &str = "(unrecognized AI workload — no metrics)";
}
```

### CAR-13: §0 mission-line template

**Surfaced by:** L25 mission-line header (Linux) / W46 mission-line
header (Windows mirror).

L25 (f15a5f7) shipped the §0 mission line header with the literal
string hardcoded:

```
edge_monitor · {n} workloads · {m} degraded · press ? for help
```

`ux_contract` v0.3.5 had no §0 mission template to source this
from. WinCleanup's W46 (Windows mirror) also hardcoded the same
string and flagged the same gap.

Contract is drafting CAR-13 for v0.3.6 to add a `mission` module
exposing the template as a const. When v0.3.6 vendors on the
Linux path-dep, L25's hardcoded string should swap to consume
`mission::TEMPLATE`.

Suggested addition to `ux_contract`:
```rust
pub mod mission {
    /// §0 mission line shown as the TUI header.
    pub const TEMPLATE: &str =
        "edge_monitor · {n} workloads · {m} degraded · press ? for help";
}
```

Resolution: tracked. Awaits v0.3.6 landing on the Linux path-dep.

## Architectural decisions to revisit

### L16 state placement: live_detail card lives in run_loop's local `Option<LiveDetailCard>` rather than on App

**Filed:** 2026-05-14 by LinuxImpl.

L16 (39eba7e) parked the live-detail card in `run_loop`'s local
`Option<LiveDetailCard>` threaded through `apply_action` and
`panels::render`, rather than adding a `live_detail` field on
`App`. The dispatch instruction "don't touch src/ui/app.rs" was
meant to protect L24's territory but was interpreted as a
permanent restriction. State placement is functionally correct
(gates green, 19 new tests pass) but architecturally
questionable — modal card state arguably belongs on `App`
alongside other card state (`postmortem`, `armed_kill`,
`history`, `show_help`).

Revisit during L17 (sparklines). If L17's rolling buffers also
need to live in `run_loop` local scope, the pattern compounds
and a refactor lifting both to `App` may be cheaper than
continuing the local-scope pattern.

Resolution: leave L16 as-is for now. L17 implementer decides
whether to absorb the refactor or continue the pattern.

## v1.1 (post-v1.0)

### Activity panel: surface AlertState raise / ack events

**Surfaced by:** L15. UX_CONTRACT.md §1 region 6 illustrates the
Activity panel with `"08:51:09  alert raised   VRAM 91%"` rows;
§4 says "Each raise + ack writes to Activity panel." The L5/L6
`AlertState::observe()` already returns `AlertEvent::Fired` /
`AlertEvent::Cleared` on transitions, but those events aren't
accumulated into RuntimeState — they're consumed and dropped at
the call site.

L15 ships the existing-data merge (run summaries + governor
audit + regressions). v1.1 adds an event buffer (likely a
`VecDeque<AlertActivityEvent>` on App or RuntimeState, bounded
by the same audit-history config) and a `build_events` source
in `panels::activity` that consumes it.

Out of L15's "existing-data merge" scope per the orchestrator's
brief; non-blocking for v1.0.

### enum migration: move `WorkloadCategory` into `ux_contract`

**Surfaced by:** L11c. Contract refined CAR-8 to const-only group
headers rather than making the enum contract-owned. For v1.0 the
enum lives in `crate::model::WorkloadCategory` and the panel maps
it locally to the contract const. v1.1+ may want to move the enum
itself to `ux_contract::WorkloadCategory` so both the Linux and
Windows binaries share the type. Mid-v1.0 refactor is out of
scope.

### telemetry: query Triton `/v2/models` to refine WorkloadCategory for hosted models
