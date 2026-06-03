//! Sprint-6 — wire-protocol types for the web companion.
//!
//! These structs are the serialization-friendly mirror of the runtime
//! state surface the web UI consumes. They live on a dedicated layer
//! so the in-memory `RuntimeState` can carry non-`Serialize` fields
//! (`Instant`, internal handles) without forcing serde derives on
//! deep internals.
//!
//! ## Wire schema v0.1 (LOCKED for Sprint-6)
//!
//! Future schema changes need contract consideration — the v2 / Altara
//! companion consumes this same JSON, so a breaking field rename
//! requires either an `v=…` versioned envelope or a coordinated
//! update across both consumers. New OPTIONAL fields are safe;
//! removed or renamed fields are not.
//!
//! ## Why a hand-written shim layer
//!
//! `RuntimeState` carries `Instant`, `Vec<ProcessSample>` (with raw
//! cmdlines + env vars), and per-tick scratch state. None of that
//! belongs on the wire. The shim types pick exactly the fields the
//! dashboard needs, no more, and convert internal types into
//! transport-friendly equivalents (e.g., `Instant` → "ms since first
//! observed" instead of an opaque handle).

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::time::Instant;

use crate::lifecycle::LifecycleSummary;
use crate::model::{AICategory, WorkloadCategory};
use crate::runtime::RuntimeState;
use crate::storage::RunRecord;
use crate::storage::run_store::ExitReason;

/// Top-level snapshot delivered on every WebSocket tick AND returned
/// by `GET /api/snapshot`. Sized for "smallish JSON" — ~5–10 KB at
/// typical workload counts. Bundles everything the dashboard needs
/// in one document so a single subscription drives the whole UI.
#[derive(Debug, Clone, Serialize)]
pub struct WireSnapshot {
    /// Monotonic tick counter. Increments by 1 per
    /// `runtime.tick_interval_ms`. Client uses this to detect skipped
    /// ticks (rare on localhost but defensible) and to anchor
    /// per-row animations to the tick cadence.
    pub tick: u64,
    /// Server-side wall clock at the moment this snapshot was
    /// composed. Surfaced to the dashboard's mission line so the
    /// browser doesn't have to trust its own clock for the timestamp
    /// shown to the operator.
    pub server_time: DateTime<Utc>,
    /// Mission-line counts.
    pub mission: WireMission,
    /// Vitals panel — host-level CPU/RAM/GPU readings.
    pub vitals: WireVitals,
    /// AI Workloads panel — one entry per AI-classified live PID,
    /// pre-grouped by workload category for the renderer's convenience.
    pub workloads: Vec<WireWorkload>,
    /// Activity feed — recent run exits / governor decisions
    /// (chronological, newest first). Capped at 50 to bound the
    /// JSON payload; the operator opens the full history view for
    /// older entries.
    pub activity: Vec<WireRunRecord>,
    /// v1.1.13 / DISPATCH 42 — currently visible alerts. Mirrors
    /// the TUI's alert region: same alerts in the same priority
    /// order, same per-entry text rendered via the same
    /// `ux_contract::alerts::*` template + `substitute(...)` pipeline.
    /// Each entry carries its pre-classified severity so the Svelte
    /// renderer maps directly to a color without re-running the
    /// `alert_tier` mapping in TypeScript — single source of truth
    /// for severity, identical to thermal v1.1.12.
    ///
    /// `#[serde(default)]` makes the field backward-compat additive:
    /// a pre-v1.1.13 wire reader deserializes the snapshot as
    /// `alerts: Vec::new()`, identical to the thermal_zones additive
    /// guarantee. Closes the v1.1.11 deferral that headless logs
    /// got alert emission but the web wire didn't.
    #[serde(default)]
    pub alerts: Vec<WireAlertEntry>,
    /// v1.2.0 / DISPATCH 45 — render-time projections from the
    /// visible alerts above. Phase 3 capstone field; each entry
    /// carries a pre-classified severity, the rendered label
    /// string, and ranked target list per the contract's
    /// `Recommendation` shape. Empty when no alerts project to
    /// recs (e.g. only `GovernorArmed` / `WorkloadExited` are
    /// visible, both suppressed by the recommendation projection).
    /// `#[serde(default)]` for the same additive guarantee
    /// `alerts` and `thermal_zones` carry.
    ///
    /// AUTHORITY LOCK: these are DISPLAY STRINGS the user reads.
    /// The wire carries no executor, no callback, no signal.
    /// `WireSuggestedAction` is the snake-case projection of the
    /// contract's discriminator-only `SuggestedAction` enum.
    #[serde(default)]
    pub recommendations: Vec<WireRecommendation>,
}

/// Mission-line counts shown at the top of the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct WireMission {
    pub workloads: usize,
    /// Subset of `workloads` whose live status is Attention or
    /// Critical. The dashboard uses this for the "N degraded" cue —
    /// matches the TUI's mission line exactly.
    pub degraded: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct WireVitals {
    /// Memory percent (0..=100). Computed via
    /// `system.memory_usage_percent()` so the web matches the TUI's
    /// vitals bar number-for-number.
    pub memory_pct: f64,
    pub memory_used_mb: u64,
    pub memory_total_mb: u64,
    pub load_average: [f64; 3],
    pub cpu_count: usize,
    pub process_count: usize,
    /// `Some(_)` when NVML reported at least one device, `None`
    /// otherwise. Mirrors the TUI's "No GPU detected" fallback
    /// without leaking the raw NVML state.
    pub gpu: Option<WireGpu>,
    /// v1.1.12 / CAR-22 — host-level thermal zones, pre-classified
    /// server-side against
    /// [`ux_contract::thresholds::THERMAL_AMBER_C`] /
    /// [`ux_contract::thresholds::THERMAL_RED_C`]. The classification
    /// happens here (not in the TS layer) so the TUI and the web read
    /// the SAME threshold constants — single source of truth, no
    /// drift if the contract bumps the values. Empty when no zones
    /// were discovered; consumers (TUI + Svelte) hide the section.
    /// Additive wire-schema bump: `#[serde(default)]` lets a pre-v1.1.12
    /// reader treat the field as `Vec::new()`.
    #[serde(default)]
    pub thermal_zones: Vec<WireThermalZone>,
}

/// v1.1.12 / CAR-22 — wire-stable shape for one thermal zone.
/// The severity is pre-classified server-side against the
/// `ux_contract::thresholds` constants, so the renderer just maps
/// the variant to a color (no `>= 85` literals in TypeScript).
#[derive(Debug, Clone, Serialize)]
pub struct WireThermalZone {
    /// Canonical zone label (e.g. `"x86_pkg_temp"`,
    /// `"cpu-thermal"`). Comes verbatim from
    /// `/sys/class/thermal/thermal_zone*/type`.
    pub label: String,
    /// Temperature in degrees Celsius. The renderer formats with
    /// one decimal place; we send the raw f32 so the formatter
    /// stays in the rendering layer.
    pub temp_celsius: f32,
    /// Pre-classified severity bucket. See the crate-private
    /// `classify_thermal` helper below for the threshold mapping.
    pub severity: WireThermalSeverity,
}

/// v1.1.12 / CAR-22 — server-side classification of a thermal
/// zone's temperature. Serialized to snake_case
/// (`"nominal" / "amber" / "red"`) so the TS reader can pattern-match
/// on string literals without importing the variant set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireThermalSeverity {
    Nominal,
    Amber,
    Red,
}

/// v1.1.12 / CAR-22 — classify a raw temperature into the
/// nominal / amber / red bucket using the contract's threshold
/// constants. Mirrors the reference implementation in
/// `ux_contract::host_vitals::tests::reference_classification_uses_thresholds`:
///
/// - `>= THERMAL_RED_C` (95.0 °C) → [`WireThermalSeverity::Red`]
/// - `>= THERMAL_AMBER_C` (85.0 °C) → [`WireThermalSeverity::Amber`]
/// - else → [`WireThermalSeverity::Nominal`]
///
/// Server-side classification means the TUI and the web render
/// against the SAME threshold values; the contract is the single
/// source of truth and there are no `>= 85` literals duplicated
/// on the consumer side.
fn classify_thermal(
    temp_celsius: f32,
    thresholds: &crate::thresholds::EffectiveThresholds,
) -> WireThermalSeverity {
    // v1.3.1 — `&EffectiveThresholds` parameter so an operator's
    // [thresholds] override reaches the wire's pre-classified
    // severity. The function stays a helper (per DISPATCH 53
    // decision 5: preserve the v1.1.12 single-source pattern). The
    // contract constants remain the deployment DEFAULTS; the
    // resolved struct shadows them per-deployment.
    let c = f64::from(temp_celsius);
    if c >= thresholds.thermal_red_c {
        WireThermalSeverity::Red
    } else if c >= thresholds.thermal_amber_c {
        WireThermalSeverity::Amber
    } else {
        WireThermalSeverity::Nominal
    }
}

/// v1.1.13 / DISPATCH 42 — wire-stable shape for one currently
/// visible alert. Mirrors the data layout of
/// `crate::ui::alerts::AlertEntry` plus a pre-classified severity
/// and a fully-rendered text body, so the Svelte renderer maps
/// directly to a color + line of text without re-running the
/// template substitution in TypeScript. Same single-source-of-truth
/// pattern as `WireThermalZone` in v1.1.12.
#[derive(Debug, Clone, Serialize)]
pub struct WireAlertEntry {
    /// Snake-case identifier of the alert (`"vram_pressure"`,
    /// `"ram_pressure"`, `"kv_pressure"`, `"governor_armed"`,
    /// `"oom_detected"`, `"workload_exited"`). Mapped from
    /// `ux_contract::AlertId` at the wire boundary via
    /// `alert_id_to_str`; the bare enum stays zero-dep on the
    /// contract side (same convention as `ActivityState` and
    /// `WorkloadStatus`).
    pub alert_id: &'static str,
    /// PID the alert is scoped to. `None` for system-scope alerts
    /// (currently only `RamPressure`).
    pub pid: Option<u32>,
    /// Workload display name. Empty string for system-scope alerts.
    pub workload_name: String,
    /// Pre-classified severity: `"attention"` or `"critical"`.
    /// Maps from `crate::ui::panels::alerts::AlertTier` via
    /// `severity_from_alert_id`. Both surfaces (TUI and Svelte)
    /// use the SAME `alert_tier` mapping; this field is the wire
    /// projection of that classification.
    pub severity: WireAlertSeverity,
    /// Fully-rendered alert text, e.g. `"VRAM at 92% — Llama-70B
    /// (PID 4523) — kill armed"`. Produced server-side by
    /// `crate::ui::panels::alerts::substitute(template_for(alert_id),
    /// entry, &live_values)` so the TUI and web show the IDENTICAL
    /// wording. The TUI banner and the Svelte alert list both read
    /// from this single source of truth.
    pub text: String,
}

/// v1.1.13 / DISPATCH 42 — server-side classification of an alert
/// to a severity bucket. Serialized to snake_case
/// (`"attention" / "critical"`) so the TS reader can pattern-match
/// on string literals — same shape as `WireThermalSeverity`.
///
/// Mirrors `crate::ui::panels::alerts::AlertTier`. Kept separate
/// from `WireThermalSeverity` because the two domains use
/// different tier vocabularies (thermal has Nominal/Amber/Red;
/// alerts have only Attention/Critical, matching the §14
/// banner color buckets — there's no "Nominal alert" because if
/// it's nominal it wouldn't be visible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireAlertSeverity {
    Attention,
    Critical,
}

/// v1.1.13 / DISPATCH 42 — snake-case wire string projection of
/// `ux_contract::AlertId`. The contract enum has no `Serialize`
/// derive (zero-dep stance, same as `ActivityState` and
/// `WorkloadStatus`); the wire boundary projects it to a stable
/// string set the Svelte dashboard pattern-matches on without
/// importing the variant set.
///
/// The string set is the IDENTICAL set used by
/// `ux_contract::alerts::*_TEMPLATE` keys in spirit (lowercase +
/// snake_case form of the variant name) — keep them in sync if a
/// future contract bump adds an alert.
fn alert_id_to_str(id: ux_contract::AlertId) -> &'static str {
    use ux_contract::AlertId;
    match id {
        AlertId::VramPressure => "vram_pressure",
        AlertId::RamPressure => "ram_pressure",
        AlertId::KvPressure => "kv_pressure",
        AlertId::GovernorArmed => "governor_armed",
        AlertId::OomDetected => "oom_detected",
        AlertId::WorkloadExited => "workload_exited",
        // v1.2.0 / DISPATCH 45 — wire spelling for the new
        // ThermalPressure variant. The Svelte AlertsPanel
        // pattern-matches on the literal `"thermal_pressure"`
        // string; keep in sync with the `severity_from_alert_id`
        // tier mapping (Attention — see `alert_tier` in
        // panels/alerts.rs).
        AlertId::ThermalPressure => "thermal_pressure",
    }
}

/// v1.1.13 / DISPATCH 42 — server-side severity classification
/// for an alert ID. Mirrors
/// `crate::ui::panels::alerts::alert_tier` exactly so the two
/// surfaces stay in sync without crossing the layer boundary
/// (calling alert_tier directly would force web → ui import; the
/// duplication here is THREE lines and intentional, and the
/// `severity_matches_tui_alert_tier` test pins the equivalence).
fn severity_from_alert_id(id: ux_contract::AlertId) -> WireAlertSeverity {
    use crate::ui::panels::alerts::{AlertTier, alert_tier};
    match alert_tier(id) {
        AlertTier::Attention => WireAlertSeverity::Attention,
        AlertTier::Critical => WireAlertSeverity::Critical,
    }
}

impl WireAlertEntry {
    /// Project one `AlertEntry` from the runtime's
    /// `RuntimeState::alerts.visible()` set onto the wire. Renders
    /// the alert text server-side via the existing
    /// `panels::alerts::substitute(template_for(id), entry,
    /// live_values_for(entry, state))` pipeline so the wire body
    /// matches the TUI banner BYTE-for-BYTE — single source of
    /// truth.
    fn from_entry(entry: &crate::ui::alerts::AlertEntry, state: &RuntimeState) -> Self {
        use crate::ui::panels::alerts::{live_values_for, substitute, template_for};
        let live = live_values_for(entry, state);
        let text = substitute(template_for(entry.alert_id), entry, &live);
        Self {
            alert_id: alert_id_to_str(entry.alert_id),
            pid: entry.pid,
            workload_name: entry.workload_name.clone(),
            severity: severity_from_alert_id(entry.alert_id),
            text,
        }
    }
}

/// v1.2.0 / DISPATCH 45 — wire-stable shape for one recommendation.
/// Mirrors the contract `Recommendation` plus a server-rendered
/// label string and a snake-case-projected action discriminator,
/// so the Svelte renderer maps directly to a color + line of
/// text without re-running the template substitution in TypeScript.
///
/// AUTHORITY LOCK: this struct is DATA. `action: WireSuggestedAction`
/// is a snake-case string projection of the contract's
/// discriminator-only `SuggestedAction` enum. There is no
/// callable, no executor handle, no signal path reachable from
/// this value.
#[derive(Debug, Clone, Serialize)]
pub struct WireRecommendation {
    /// Snake-case identifier of the underlying alert
    /// (`"vram_pressure"`, `"thermal_pressure"`, etc.). Lets the
    /// dashboard correlate a rec with the alert it derives from
    /// for stylistic grouping.
    pub alert_id: &'static str,
    /// `"workload"` or `"system"`. Mirrors
    /// `ux_contract::recommendation::RecommendationScope`.
    pub scope: &'static str,
    /// Pre-classified severity: `"info" / "warning" / "critical"`.
    /// Mirrors `RecommendationSeverity`. Drives ordering and
    /// color in the renderer.
    pub severity: WireRecommendationSeverity,
    /// Snake-case action discriminator:
    /// `"consider_kill" / "consider_reduce_load" / "consider_restart"`.
    /// The Svelte dashboard pattern-matches on this string for
    /// any per-action styling beyond severity.
    pub action: WireSuggestedAction,
    /// Ranked targets. Empty for system-scope recs without
    /// per-PID attribution (thermal).
    pub targets: Vec<WireRecommendedTarget>,
    /// Server-rendered label (template substituted server-side
    /// via `recommend::render_label`). The Svelte renderer shows
    /// this string verbatim; NO template substitution in TS.
    pub label: String,
    /// Producer-formatted rationale rendered as a one-line
    /// sub-text under the label. Single source of truth for
    /// rationale wording across TUI and web.
    pub reason: String,
}

/// v1.2.0 / DISPATCH 45 — one ranked target of a recommendation.
/// Carries the PID, the display name, and an optional
/// metric-snapshot evidence string captured at fire time.
#[derive(Debug, Clone, Serialize)]
pub struct WireRecommendedTarget {
    pub pid: u32,
    pub name: String,
    pub evidence: Option<String>,
}

/// v1.2.0 / DISPATCH 45 — server-side projection of the
/// contract's `RecommendationSeverity` enum to a snake-case wire
/// string. Mirrors the `WireAlertSeverity` / `WireThermalSeverity`
/// pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireRecommendationSeverity {
    Info,
    Warning,
    Critical,
}

/// v1.2.0 / DISPATCH 45 — server-side projection of the
/// contract's `SuggestedAction` enum.
///
/// AUTHORITY LOCK: this is a snake-case STRING projection. There
/// is no `WireSuggestedAction::execute()`, no callback, no signal
/// path reachable from a value of this type. The Svelte dashboard
/// pattern-matches on the literal for styling; the action a
/// recommendation suggests is always taken via the operator's
/// existing manual flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireSuggestedAction {
    ConsiderKill,
    ConsiderReduceLoad,
    ConsiderRestart,
}

impl WireRecommendation {
    /// Project one contract `Recommendation` onto the wire.
    /// Calls `crate::recommend::render_label` for the
    /// server-rendered label string so the TUI and web read
    /// the IDENTICAL label.
    ///
    /// AUTHORITY LOCK: input is a `&Recommendation` (pure data
    /// from the projection). No executor, no callback, no signal
    /// path. Output is `WireRecommendation` (pure data). The
    /// `recommendation_path_has_no_actuation_handle` test in C5
    /// guards the module-level invariant.
    fn from_rec(rec: &ux_contract::recommendation::Recommendation) -> Self {
        use ux_contract::recommendation::{
            RecommendationScope, RecommendationSeverity, SuggestedAction,
        };
        let severity = match rec.severity {
            RecommendationSeverity::Info => WireRecommendationSeverity::Info,
            RecommendationSeverity::Warning => WireRecommendationSeverity::Warning,
            RecommendationSeverity::Critical => WireRecommendationSeverity::Critical,
        };
        let action = match rec.action {
            SuggestedAction::ConsiderKill => WireSuggestedAction::ConsiderKill,
            SuggestedAction::ConsiderReduceLoad => WireSuggestedAction::ConsiderReduceLoad,
            SuggestedAction::ConsiderRestart => WireSuggestedAction::ConsiderRestart,
        };
        let scope = match rec.scope {
            RecommendationScope::Workload => "workload",
            RecommendationScope::System => "system",
        };
        let targets = rec
            .targets
            .iter()
            .map(|t| WireRecommendedTarget {
                pid: t.pid,
                name: t.name.clone(),
                evidence: t.evidence.clone(),
            })
            .collect();
        let label = crate::recommend::render_label(rec);
        Self {
            alert_id: alert_id_to_str(rec.alert_id),
            scope,
            severity,
            action,
            targets,
            label,
            reason: rec.reason.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WireGpu {
    pub vram_pct: f64,
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
    pub device_count: usize,
}

/// One AI workload row.
#[derive(Debug, Clone, Serialize)]
pub struct WireWorkload {
    pub pid: u32,
    pub name: String,
    /// Resolved model name when the classifier extracted one — e.g.
    /// `phi3-mini-q8_0` — otherwise `None`. The dashboard renders
    /// the resolved name when present, falling back to `name` so a
    /// just-spawned process without a fingerprint doesn't render as
    /// blank.
    pub model_name: Option<String>,
    /// `AICategory` as a wire-stable string. Using strings rather
    /// than the bare enum repr means the JSON survives an enum-variant
    /// reorder on the Rust side without bumping the wire schema.
    pub category: String,
    pub workload_category: String,
    pub cpu_pct: f32,
    pub rss_mb: u64,
    /// v1.0.1 B-NEW-9 (operator request) — RSS as a percentage of the
    /// host's total RAM. `None` when the platform layer hasn't yet
    /// surfaced a `total_memory > 0` snapshot (first tick on a fresh
    /// process, or a sysinfo failure). The dashboard renders this as
    /// "121M (0.4%)"; an operator quoted absolute megabytes only and
    /// asked "is that a lot?" — the percentage answers that for them.
    pub ram_pct: Option<f32>,
    /// VRAM in MB. `None` when the process has no GPU allocation
    /// (vision/LLM workloads that haven't loaded yet, ROS2 nodes
    /// that don't touch GPU).
    pub vram_mb: Option<u64>,
    /// Live tokens/sec for LLM workloads sampled passively via vLLM /
    /// llama.cpp Prometheus. `None` for non-LLM categories OR LLM
    /// processes whose sampler hasn't reported a value yet.
    pub tokens_per_sec: Option<f32>,
    /// Live frames/sec for Vision workloads (B6 conditional).
    pub fps: Option<f32>,
    /// Live KV-cache occupancy percent for LLM workloads.
    pub kv_cache_peak_pct: Option<f32>,
    /// Status enum mirrored as a wire-stable string —
    /// `healthy`/`attention`/`critical`/`loading`. The dashboard
    /// renders the status dot color from this.
    pub status: String,
    /// Phase 2 / DISPATCH 1 — per-category activity state as a
    /// wire-stable string. One of `active`/`idle`/`loading`/
    /// `not_detected`. `None` when no Phase-2 sampler has surfaced a
    /// state for this PID yet (cold start, or a workload category
    /// with no Phase-2 sampler — vLLM / llama.cpp continue to
    /// report throughput-only). Web UI hides the column when every
    /// visible row's `activity` is `None`, mirroring the TUI.
    pub activity: Option<String>,
}

/// One completed-run record, suitable for both the history view and
/// the activity feed. Built from `RunRecord` (which is itself built
/// from `LifecycleSummary`) so every record represents an exited
/// workload — see B13 invariant in `tests/history_refactor.rs`.
#[derive(Debug, Clone, Serialize)]
pub struct WireRunRecord {
    pub pid: u32,
    pub name: String,
    pub model_name: Option<String>,
    pub spawn_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub uptime_secs: u64,
    pub avg_cpu_pct: f32,
    pub peak_cpu_pct: f32,
    pub peak_rss_mb: u64,
    pub peak_vram_mb: u64,
    /// Human-readable exit kind — `clean` / `governor` / `oom` /
    /// `crash` / `cuda` / `segfault` / `signal` / `unknown`.
    /// Stays a string for schema stability; the variants follow
    /// `ExitReason` but the names are pinned here.
    pub exit_kind: String,
    /// Detail string when the exit kind carries one (signal number,
    /// exit code, CUDA error message). `None` for plain `clean`.
    pub exit_detail: Option<String>,
}

impl WireSnapshot {
    /// Compose a wire snapshot from the runtime's authoritative state
    /// plus a slice of recent completed RunRecords. The recent slice
    /// is the caller's choice — Sprint-6 main.rs hands in
    /// `runtime.recent_completed(50)` to keep JSON bounded.
    pub fn from_runtime_state(state: &RuntimeState, recent: &[RunRecord]) -> Self {
        let mission = WireMission::from_runtime(state);
        let vitals = WireVitals::from_runtime(state);
        let workloads = state
            .ai_processes()
            .map(|p| WireWorkload::from_annotated(p, state))
            .collect::<Vec<_>>();
        let activity = recent.iter().map(WireRunRecord::from_record).collect();
        // v1.1.13 / DISPATCH 42 — project the same Active alert set
        // the TUI banner reads. `AlertState::visible()` returns
        // alerts in §4 priority order (Critical before Attention);
        // we preserve that order on the wire so the Svelte renderer
        // can render top-to-bottom without re-sorting.
        let alerts = state
            .alerts
            .visible()
            .iter()
            .map(|entry| WireAlertEntry::from_entry(entry, state))
            .collect::<Vec<_>>();
        // v1.2.0 / DISPATCH 45 — recommendation projection rides
        // alongside the alerts. `recommend::project_recommendations`
        // is the SAME derived view both surfaces consume; the TUI
        // calls it directly, the wire calls it here. Both pass
        // a read-only `&RuntimeState` and receive a `Vec` of pure
        // data values — no executor, no callback.
        let recommendations = crate::recommend::project_recommendations(state)
            .iter()
            .map(WireRecommendation::from_rec)
            .collect::<Vec<_>>();
        Self {
            tick: state.tick_count,
            server_time: Utc::now(),
            mission,
            vitals,
            workloads,
            activity,
            alerts,
            recommendations,
        }
    }

    /// Empty snapshot for the watch channel's initial value. Lets
    /// the first WS client get something coherent before the first
    /// tick runs through `from_runtime_state`.
    pub fn empty() -> Self {
        Self {
            tick: 0,
            server_time: Utc::now(),
            mission: WireMission {
                workloads: 0,
                degraded: 0,
            },
            vitals: WireVitals {
                memory_pct: 0.0,
                memory_used_mb: 0,
                memory_total_mb: 0,
                load_average: [0.0, 0.0, 0.0],
                cpu_count: 0,
                process_count: 0,
                gpu: None,
                thermal_zones: Vec::new(),
            },
            workloads: Vec::new(),
            activity: Vec::new(),
            alerts: Vec::new(),
            recommendations: Vec::new(),
        }
    }
}

impl WireMission {
    fn from_runtime(state: &RuntimeState) -> Self {
        // We compute degraded by walking workloads twice (count vs
        // filter) rather than re-running `compute_workload_status`
        // here because the TUI uses `panels::workloads::ordered_rows`
        // which already produces the canonical status for each row.
        // For the web wire path we replicate the simpler check:
        // degraded = matches Attention/Critical.
        let workloads: Vec<&crate::runtime::AnnotatedProcess> = state.ai_processes().collect();
        let total = workloads.len();
        let degraded = workloads
            .iter()
            .filter(|p| {
                let inputs = build_status_inputs(p, state);
                let status =
                    crate::runtime::compute_workload_status(&inputs, &state.thresholds);
                matches!(
                    status,
                    ux_contract::WorkloadStatus::Attention | ux_contract::WorkloadStatus::Critical
                )
            })
            .count();
        Self {
            workloads: total,
            degraded,
        }
    }
}

impl WireVitals {
    fn from_runtime(state: &RuntimeState) -> Self {
        let Some(snap) = state.last_snapshot.as_ref() else {
            return Self {
                memory_pct: 0.0,
                memory_used_mb: 0,
                memory_total_mb: 0,
                load_average: [0.0, 0.0, 0.0],
                cpu_count: 0,
                process_count: 0,
                gpu: None,
                thermal_zones: Vec::new(),
            };
        };
        let memory_total_mb = snap.system.total_memory / (1024 * 1024);
        let memory_used_mb = snap.system.used_memory / (1024 * 1024);
        let memory_pct = snap.system.memory_usage_percent();
        let gpu = if snap.gpu.has_gpu() {
            let total = snap.gpu.total_vram_all_devices();
            let used = snap.gpu.used_vram_all_devices();
            let pct = if total > 0 {
                (used as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            Some(WireGpu {
                vram_pct: pct,
                vram_used_mb: used / (1024 * 1024),
                vram_total_mb: total / (1024 * 1024),
                device_count: snap.gpu.devices.len(),
            })
        } else {
            None
        };
        // v1.1.12 / CAR-22 — pre-classify server-side. The thermal
        // zones come off the platform-layer host_vitals collection
        // already sorted by label; we map each into a
        // `WireThermalZone` with its severity attached.
        let thermal_zones: Vec<WireThermalZone> = snap
            .vitals
            .thermal_zones
            .iter()
            .map(|z| WireThermalZone {
                label: z.label.clone(),
                temp_celsius: z.temp_celsius,
                severity: classify_thermal(z.temp_celsius, &state.thresholds),
            })
            .collect();
        Self {
            memory_pct,
            memory_used_mb,
            memory_total_mb,
            load_average: snap.system.load_average,
            cpu_count: snap.system.cpu_count,
            process_count: snap.processes.len(),
            gpu,
            thermal_zones,
        }
    }
}

impl WireWorkload {
    fn from_annotated(p: &crate::runtime::AnnotatedProcess, state: &RuntimeState) -> Self {
        let vram_mb = p.vram_bytes.map(|b| b / (1024 * 1024)).filter(|&v| v > 0);
        let lt = state.live_telemetry.get(&p.pid);
        let tokens_per_sec = lt.and_then(|t| t.tokens_per_sec_avg);
        let fps = lt.and_then(|t| t.fps_avg);
        let kv_cache_peak_pct = lt.and_then(|t| t.kv_cache_peak_pct);
        let activity = lt
            .and_then(|t| t.activity)
            .map(|a| activity_state_to_str(a).to_string());
        let total_ram_bytes = state
            .last_snapshot
            .as_ref()
            .map(|s| s.system.total_memory)
            .filter(|&t| t > 0);
        let ram_pct = compute_ram_pct(p.rss_mb, total_ram_bytes);
        let inputs = build_status_inputs(p, state);
        let status = crate::runtime::compute_workload_status(&inputs, &state.thresholds);
        Self {
            pid: p.pid,
            name: p.name.clone(),
            // Sprint-7 Item 2 — humanize ollama's sha256-XXX blob
            // names before serializing; the wire mirror of the
            // TUI's row-formatting decision.
            model_name: p
                .model_name
                .as_deref()
                .map(crate::model::humanize_model_name),
            category: category_to_str(p.category).to_string(),
            workload_category: workload_category_to_str(p.workload_category).to_string(),
            cpu_pct: p.cpu_pct,
            rss_mb: p.rss_mb,
            ram_pct,
            vram_mb,
            tokens_per_sec,
            fps,
            kv_cache_peak_pct,
            status: workload_status_to_str(status).to_string(),
            activity,
        }
    }
}

/// Phase 2 / DISPATCH 1 — wire-stable string projection of the local
/// `ActivityState` enum. Kept here rather than relying on serde's
/// `rename_all = "snake_case"` so the wire schema is decoupled from
/// the enum's `Serialize` impl (lift-to-`ux_contract::activity` won't
/// break the dashboard's expected string set).
fn activity_state_to_str(state: ux_contract::activity::ActivityState) -> &'static str {
    use ux_contract::activity::ActivityState;
    match state {
        ActivityState::Active => "active",
        ActivityState::Idle => "idle",
        ActivityState::Loading => "loading",
        ActivityState::NotDetected => "not_detected",
    }
}

/// v1.0.1 B-NEW-9 — pure helper so the percentage rule is testable
/// without spinning up an AnnotatedProcess + RuntimeState. None
/// signals "platform layer has no total to divide against"; the
/// dashboard then falls back to bare megabytes.
pub(crate) fn compute_ram_pct(rss_mb: u64, total_ram_bytes: Option<u64>) -> Option<f32> {
    let total = total_ram_bytes?;
    if total == 0 {
        return None;
    }
    let rss_bytes = rss_mb.saturating_mul(1024 * 1024) as f64;
    Some(((rss_bytes / total as f64) * 100.0) as f32)
}

impl WireRunRecord {
    /// Build from a persisted `RunRecord`. The `exit_kind` /
    /// `exit_detail` projection collapses the rich `ExitReason` enum
    /// to a stable two-string pair so the wire schema doesn't track
    /// every variant the reason taxonomy might add.
    pub fn from_record(rec: &RunRecord) -> Self {
        let (kind, detail) = match &rec.exit_reason {
            ExitReason::CleanExit => ("clean", None),
            ExitReason::UserSignal { signal } => ("signal", Some(format!("signal {signal}"))),
            ExitReason::GovernorKill { reason } => ("governor", Some(reason.clone())),
            ExitReason::Segfault => ("segfault", None),
            ExitReason::OutOfMemory { ram, vram } => {
                let detail = match (ram, vram) {
                    (true, true) => "RAM and GPU memory",
                    (true, false) => "RAM",
                    (false, true) => "GPU memory",
                    (false, false) => "unknown",
                };
                ("oom", Some(detail.to_string()))
            }
            ExitReason::CudaError { last_msg } => {
                ("cuda", last_msg.clone())
            }
            ExitReason::Crash { exit_code } => ("crash", Some(format!("exit {exit_code}"))),
            ExitReason::Unknown => ("unknown", None),
        };
        Self::from_summary_with_exit(&rec.summary, kind.to_string(), detail)
    }

    fn from_summary_with_exit(
        summary: &LifecycleSummary,
        kind: String,
        detail: Option<String>,
    ) -> Self {
        Self {
            pid: summary.pid,
            name: summary.name.clone(),
            // Sprint-7 Item 2 — same humanization as the live row.
            model_name: summary
                .model_name
                .as_deref()
                .map(crate::model::humanize_model_name),
            spawn_time: summary.spawn_time,
            exit_time: summary.exit_time,
            uptime_secs: summary.uptime_secs.max(0) as u64,
            avg_cpu_pct: summary.avg_cpu_pct,
            peak_cpu_pct: summary.peak_cpu_pct,
            peak_rss_mb: summary.peak_rss_mb,
            peak_vram_mb: summary.peak_vram_mb,
            exit_kind: kind,
            exit_detail: detail,
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────

fn build_status_inputs(
    proc: &crate::runtime::AnnotatedProcess,
    state: &RuntimeState,
) -> crate::runtime::WorkloadStatusInputs {
    let total_vram = state
        .last_snapshot
        .as_ref()
        .map(|s| s.gpu.total_vram_all_devices())
        .filter(|&v| v > 0);
    let vram_pct = match (total_vram, proc.vram_bytes) {
        (Some(total), Some(used)) => Some((used as f64 / total as f64) * 100.0),
        _ => None,
    };
    let ram_pct = state
        .last_snapshot
        .as_ref()
        .map(|s| s.system.memory_usage_percent());
    let kv_cache_pct = state
        .live_telemetry
        .get(&proc.pid)
        .and_then(|lt| lt.kv_cache_peak_pct.map(|v| v as f64));
    let telemetry_age = Instant::now().saturating_duration_since(proc.first_observed_at);
    crate::runtime::WorkloadStatusInputs {
        vram_pct,
        ram_pct,
        kv_cache_pct,
        throughput_vs_baseline: None,
        governor_armed: false,
        oom_detected: false,
        telemetry_age,
    }
}

fn category_to_str(c: AICategory) -> &'static str {
    match c {
        AICategory::Inference => "inference",
        AICategory::Training => "training",
        AICategory::ModelDownload => "model_download",
        AICategory::Framework => "framework",
        AICategory::NotAi => "not_ai",
    }
}

fn workload_category_to_str(c: WorkloadCategory) -> &'static str {
    match c {
        WorkloadCategory::LLM => "llm",
        // Sprint-7.5 / CAR-18 — Agent maps to the lowercase token
        // the frontend expects in `WorkloadsPanel.svelte::ORDER`.
        WorkloadCategory::Agent => "agent",
        WorkloadCategory::Vision => "vision",
        WorkloadCategory::ROS2 => "ros2",
        WorkloadCategory::Embeddings => "embeddings",
        WorkloadCategory::Unknown => "unknown",
    }
}

fn workload_status_to_str(s: ux_contract::WorkloadStatus) -> &'static str {
    match s {
        ux_contract::WorkloadStatus::Healthy => "healthy",
        ux_contract::WorkloadStatus::Attention => "attention",
        ux_contract::WorkloadStatus::Critical => "critical",
        ux_contract::WorkloadStatus::Loading => "loading",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runtime::Runtime;

    #[test]
    fn empty_snapshot_serializes_to_valid_json() {
        let snap = WireSnapshot::empty();
        let json = serde_json::to_string(&snap).expect("serialize");
        // Round-trip back via Value so we can poke at the shape.
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(v["tick"].as_u64(), Some(0));
        assert_eq!(v["mission"]["workloads"].as_u64(), Some(0));
        assert_eq!(v["mission"]["degraded"].as_u64(), Some(0));
        assert!(v["workloads"].is_array());
        assert!(v["activity"].is_array());
        assert!(v["vitals"]["gpu"].is_null());
        // v1.1.12 — thermal_zones is additive and ships as `[]` on
        // the empty wire payload.
        assert!(
            v["vitals"]["thermal_zones"].is_array(),
            "v1.1.12 wire schema must carry thermal_zones as an array",
        );
        assert_eq!(
            v["vitals"]["thermal_zones"].as_array().unwrap().len(),
            0,
            "empty snapshot must carry an empty thermal_zones list",
        );
        // v1.1.13 — alerts is the new additive field; ships as
        // `[]` on the empty wire payload.
        assert!(
            v["alerts"].is_array(),
            "v1.1.13 wire schema must carry alerts as an array",
        );
        assert_eq!(
            v["alerts"].as_array().unwrap().len(),
            0,
            "empty snapshot must carry an empty alerts list",
        );
        // v1.2.0 — recommendations is the new additive field;
        // same additive-default shape as alerts and thermal_zones.
        assert!(
            v["recommendations"].is_array(),
            "v1.2.0 wire schema must carry recommendations as an array",
        );
        assert_eq!(
            v["recommendations"].as_array().unwrap().len(),
            0,
            "empty snapshot must carry an empty recommendations list",
        );
    }

    /// v1.2.0 / DISPATCH 45 — end-to-end: drive AlertState with a
    /// VRAM-pressure alert + a thermal scenario, compose a wire
    /// snapshot, and assert the recommendations field carries
    /// the projected recs with snake-case action/scope/severity
    /// fields the Svelte dashboard pattern-matches on.
    ///
    /// AUTHORITY LOCK: this test exercises the wire mapping; no
    /// kill / signal / actuation reached.
    #[test]
    fn recommendations_project_to_wire() {
        use crate::ui::alerts::WorkloadRef;
        use std::time::{Duration, Instant};
        use ux_contract::AlertId;

        let mut runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let now = Instant::now();
        // VRAM pressure on a single workload → ConsiderKill, Warning.
        runtime.state_mut().alerts.observe(
            now,
            WorkloadRef::workload(4523, "Llama-70B"),
            AlertId::VramPressure,
            true,
        );
        runtime.state_mut().alerts.observe(
            now + Duration::from_secs(5),
            WorkloadRef::workload(4523, "Llama-70B"),
            AlertId::VramPressure,
            true,
        );

        let snap = WireSnapshot::from_runtime_state(runtime.state(), &[]);
        assert!(
            !snap.recommendations.is_empty(),
            "VRAM pressure alert MUST project to a wire recommendation",
        );
        let r = &snap.recommendations[0];
        assert_eq!(r.alert_id, "vram_pressure");
        assert_eq!(r.scope, "workload");
        assert_eq!(r.severity, WireRecommendationSeverity::Warning);
        assert_eq!(r.action, WireSuggestedAction::ConsiderKill);
        assert_eq!(r.targets.len(), 1);
        assert_eq!(r.targets[0].pid, 4523);
        // Label was rendered server-side via `recommend::render_label`
        // — the Svelte renderer reads `label` verbatim.
        assert!(r.label.contains("PID 4523"), "label: {}", r.label);
        assert!(r.label.contains("Llama-70B"), "label: {}", r.label);

        // Full snapshot round-trips through JSON cleanly (TS
        // consumer's parser path).
        let json = serde_json::to_string(&snap).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert_eq!(
            v["recommendations"].as_array().unwrap().len(),
            1,
            "recommendation must round-trip through JSON",
        );
        // Snake-case spellings (drive the TS pattern-match).
        assert_eq!(v["recommendations"][0]["action"], "consider_kill");
        assert_eq!(v["recommendations"][0]["severity"], "warning");
        assert_eq!(v["recommendations"][0]["scope"], "workload");
    }

    /// v1.2.0 / DISPATCH 45 — pin the snake-case JSON shape for
    /// `WireSuggestedAction`. The Svelte client matches on the
    /// literal strings; a future `rename_all` regression here
    /// would split the surface across the two consumers.
    #[test]
    fn suggested_action_serializes_snake_case() {
        let cases: &[(WireSuggestedAction, &str)] = &[
            (WireSuggestedAction::ConsiderKill, "\"consider_kill\""),
            (WireSuggestedAction::ConsiderReduceLoad, "\"consider_reduce_load\""),
            (WireSuggestedAction::ConsiderRestart, "\"consider_restart\""),
        ];
        for (action, expected) in cases {
            let json = serde_json::to_string(action).expect("serialize");
            assert_eq!(&json, expected, "{action:?} → {expected}");
        }
    }

    /// v1.2.0 / DISPATCH 45 — pin the snake-case JSON shape for
    /// `WireRecommendationSeverity`.
    #[test]
    fn recommendation_severity_serializes_snake_case() {
        let cases: &[(WireRecommendationSeverity, &str)] = &[
            (WireRecommendationSeverity::Info, "\"info\""),
            (WireRecommendationSeverity::Warning, "\"warning\""),
            (WireRecommendationSeverity::Critical, "\"critical\""),
        ];
        for (sev, expected) in cases {
            let json = serde_json::to_string(sev).expect("serialize");
            assert_eq!(&json, expected, "{sev:?} → {expected}");
        }
    }

    /// v1.1.13 / DISPATCH 42 — `WireSnapshot.alerts` is an additive
    /// field protected by `#[serde(default)]`. `WireSnapshot` is
    /// `Serialize`-only in this codebase (the watch-channel
    /// publisher serializes; the TS client deserializes — no
    /// round-trip on the Rust side), so the additive guarantee
    /// is forward-looking: it ensures any future `Deserialize`
    /// derive (or a test/import that wants to roundtrip) sees the
    /// missing-field case as `Vec::new()` instead of an error.
    ///
    /// Pins:
    ///
    /// 1. `WireSnapshot::empty().alerts` is an empty vec (Rust-side
    ///    construction respects the additive default).
    /// 2. Serializing the empty snapshot emits an `"alerts": []`
    ///    field (the TS client sees an array, never `undefined`,
    ///    when this binary is the server).
    ///
    /// The forward-looking deserialize property is documented but
    /// not run here because there's no Deserialize derive to
    /// exercise — the `#[serde(default)]` annotation in the struct
    /// header carries the guarantee at the type level.
    #[test]
    fn wiresnapshot_alerts_additive_default() {
        // (1) Rust-side construction: empty() produces empty alerts.
        let snap = WireSnapshot::empty();
        assert!(
            snap.alerts.is_empty(),
            "WireSnapshot::empty() must materialise `alerts` as an \
             empty vec — the additive-default guarantee on the \
             Rust side.",
        );
        // (2) Wire emission: the empty vec serializes as `"alerts":
        // []` (not omitted), so a current-version TS client always
        // sees an array, even on the empty initial payload.
        let json = serde_json::to_string(&snap).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
        assert!(
            v["alerts"].is_array(),
            "serialized empty snapshot must emit `alerts` as an array, \
             not omit the field — TS dashboard's `alerts?` field is \
             then always non-undefined on a current-version server.",
        );
        assert_eq!(v["alerts"].as_array().unwrap().len(), 0);
    }

    /// v1.1.13 / DISPATCH 42 — `severity_from_alert_id` MUST agree
    /// with `crate::ui::panels::alerts::alert_tier` for every
    /// `AlertId` variant. The duplication exists because the wire
    /// layer doesn't import `ui/panels` for ranking — the cost is
    /// three short lines kept in sync, and THIS test pins the sync.
    /// If a future contract bump adds an `AlertId`, the exhaustive
    /// match in `alert_id_to_str` and `severity_from_alert_id`
    /// breaks at compile time, and this test breaks at run time
    /// for any new variant whose tier doesn't round-trip cleanly.
    #[test]
    fn severity_matches_tui_alert_tier() {
        use crate::ui::panels::alerts::{AlertTier, alert_tier};
        use ux_contract::AlertId;
        let cases: &[AlertId] = &[
            AlertId::VramPressure,
            AlertId::RamPressure,
            AlertId::KvPressure,
            AlertId::GovernorArmed,
            AlertId::OomDetected,
            AlertId::WorkloadExited,
        ];
        for id in cases {
            let wire = severity_from_alert_id(*id);
            let tui = alert_tier(*id);
            let expected_wire = match tui {
                AlertTier::Attention => WireAlertSeverity::Attention,
                AlertTier::Critical => WireAlertSeverity::Critical,
            };
            assert_eq!(
                wire, expected_wire,
                "severity_from_alert_id({id:?}) MUST match alert_tier({id:?}) \
                 — drift between wire and TUI severity would split the \
                 banner color across the two surfaces.",
            );
        }
    }

    /// v1.1.13 / DISPATCH 42 — the snake-case wire string set must
    /// cover EVERY `AlertId` variant (no unknown left over) and
    /// produce the IDENTICAL identifier the TS dashboard
    /// pattern-matches on. Compile-time exhaustiveness guards the
    /// "no missing variant" property; this test guards the
    /// "stable string spelling" property.
    #[test]
    fn alert_id_to_str_covers_all_variants_with_snake_case() {
        use ux_contract::AlertId;
        let cases: &[(AlertId, &str)] = &[
            (AlertId::VramPressure, "vram_pressure"),
            (AlertId::RamPressure, "ram_pressure"),
            (AlertId::KvPressure, "kv_pressure"),
            (AlertId::GovernorArmed, "governor_armed"),
            (AlertId::OomDetected, "oom_detected"),
            (AlertId::WorkloadExited, "workload_exited"),
        ];
        for (id, expected) in cases {
            assert_eq!(
                alert_id_to_str(*id),
                *expected,
                "alert_id_to_str({id:?}) must produce {expected}",
            );
        }
    }

    /// v1.1.13 / DISPATCH 42 — `WireAlertSeverity` serializes to
    /// snake_case strings (`"attention" / "critical"`) so the TS
    /// reader can pattern-match on literals. Same shape as
    /// `WireThermalSeverity`. Pin against a future
    /// `rename_all` regression.
    #[test]
    fn alert_severity_serializes_snake_case() {
        let cases: &[(WireAlertSeverity, &str)] = &[
            (WireAlertSeverity::Attention, "\"attention\""),
            (WireAlertSeverity::Critical, "\"critical\""),
        ];
        for (sev, expected) in cases {
            let json = serde_json::to_string(sev).expect("serialize");
            assert_eq!(&json, expected, "{sev:?} → {expected}");
        }
    }

    /// v1.1.13 / DISPATCH 42 — full end-to-end pin:
    /// `AlertState::visible()` → `Vec<WireAlertEntry>` projection
    /// preserves alert_id, pid, workload_name, classifies severity
    /// correctly, and renders the same text the TUI banner would.
    /// Drives `RuntimeState` directly with two synthetic alerts (one
    /// instant-fire Critical, one sustain-gated Attention via the
    /// observe-twice pattern), then composes a wire snapshot and
    /// inspects the projected vector.
    #[test]
    fn alerts_serialize_to_wire() {
        use crate::ui::alerts::WorkloadRef;
        use std::time::{Duration, Instant};
        use ux_contract::AlertId;

        let mut runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let state = runtime.state_mut();
        let t0 = Instant::now();
        // (1) Instant-fire Critical: GovernorArmed on PID 206 / phi3.
        state.alerts.observe(
            t0,
            WorkloadRef::workload(206, "phi3"),
            AlertId::GovernorArmed,
            true,
        );
        // (2) Sustain-gated Attention: VramPressure on PID 4523 /
        // Llama-70B. Observe twice across the sustain window.
        state.alerts.observe(
            t0,
            WorkloadRef::workload(4523, "Llama-70B"),
            AlertId::VramPressure,
            true,
        );
        state.alerts.observe(
            t0 + Duration::from_secs(5),
            WorkloadRef::workload(4523, "Llama-70B"),
            AlertId::VramPressure,
            true,
        );

        let snap = WireSnapshot::from_runtime_state(runtime.state(), &[]);
        assert_eq!(
            snap.alerts.len(),
            2,
            "both Active alerts must surface on the wire (got {})",
            snap.alerts.len(),
        );

        // Find each by alert_id literal (the priority order is
        // governed by AlertState::visible()).
        let armed = snap
            .alerts
            .iter()
            .find(|a| a.alert_id == "governor_armed")
            .expect("governor_armed alert on wire");
        assert_eq!(armed.pid, Some(206));
        assert_eq!(armed.workload_name, "phi3");
        assert_eq!(armed.severity, WireAlertSeverity::Critical);
        assert!(
            !armed.text.is_empty(),
            "WireAlertEntry.text must be the rendered banner string",
        );

        let vram = snap
            .alerts
            .iter()
            .find(|a| a.alert_id == "vram_pressure")
            .expect("vram_pressure alert on wire");
        assert_eq!(vram.pid, Some(4523));
        assert_eq!(vram.workload_name, "Llama-70B");
        assert_eq!(vram.severity, WireAlertSeverity::Attention);

        // The whole snapshot round-trips through JSON cleanly.
        let json = serde_json::to_string(&snap).expect("serialize wire");
        let v: serde_json::Value = serde_json::from_str(&json).expect("parse wire");
        assert_eq!(
            v["alerts"].as_array().unwrap().len(),
            2,
            "alerts must round-trip through JSON intact",
        );
    }

    /// v1.1.12 / CAR-22 — pin the boundary semantics of the
    /// server-side classifier. `>=` semantics: the threshold value
    /// itself is the LOWER edge of the next bucket. Mirrors the
    /// contract-side reference implementation at
    /// `ux_contract::host_vitals::tests::reference_classification_uses_thresholds`
    /// so a future ux_contract bump that tweaks the thresholds is
    /// caught on the consumer side too.
    #[test]
    fn classify_thermal_boundaries() {
        use ux_contract::thresholds::{THERMAL_AMBER_C, THERMAL_RED_C};

        // Below amber → Nominal. Just under the boundary AND well
        // under the boundary.
        assert_eq!(classify_thermal(45.0, &crate::thresholds::EffectiveThresholds::default()), WireThermalSeverity::Nominal);
        assert_eq!(
            classify_thermal(THERMAL_AMBER_C as f32 - 0.1, &crate::thresholds::EffectiveThresholds::default()),
            WireThermalSeverity::Nominal,
            "84.9 °C must classify as Nominal (just below the amber \
             threshold)",
        );
        // Amber boundary: `>=` so 85.0 itself is Amber.
        assert_eq!(
            classify_thermal(THERMAL_AMBER_C as f32, &crate::thresholds::EffectiveThresholds::default()),
            WireThermalSeverity::Amber,
            "85.0 °C must classify as Amber (the threshold value \
             itself is the lower edge of the amber bucket)",
        );
        // Just below red → still Amber.
        assert_eq!(
            classify_thermal(THERMAL_RED_C as f32 - 0.1, &crate::thresholds::EffectiveThresholds::default()),
            WireThermalSeverity::Amber,
            "94.9 °C must classify as Amber (just below the red \
             threshold)",
        );
        // Red boundary: `>=` so 95.0 itself is Red.
        assert_eq!(
            classify_thermal(THERMAL_RED_C as f32, &crate::thresholds::EffectiveThresholds::default()),
            WireThermalSeverity::Red,
            "95.0 °C must classify as Red (the threshold value \
             itself is the lower edge of the red bucket)",
        );
        // Well above red.
        assert_eq!(classify_thermal(105.0, &crate::thresholds::EffectiveThresholds::default()), WireThermalSeverity::Red);
    }

    /// v1.1.12 / CAR-22 — server-side classification surfaces on the
    /// JSON wire as snake_case strings (`"nominal" / "amber" /
    /// "red"`). The TS layer pattern-matches on those literals; this
    /// test pins the serialization shape so a future
    /// `rename_all` regression is caught here.
    #[test]
    fn thermal_severity_serializes_snake_case() {
        let cases: &[(WireThermalSeverity, &str)] = &[
            (WireThermalSeverity::Nominal, "\"nominal\""),
            (WireThermalSeverity::Amber, "\"amber\""),
            (WireThermalSeverity::Red, "\"red\""),
        ];
        for (sev, expected) in cases {
            let json = serde_json::to_string(sev).expect("serialize");
            assert_eq!(&json, expected, "{sev:?} → {expected}");
        }
    }

    #[test]
    fn snapshot_from_default_runtime_has_zero_workloads() {
        let runtime = Runtime::new(Config::default()).expect("Runtime::new must succeed with contract default config");
        let snap = WireSnapshot::from_runtime_state(runtime.state(), &[]);
        assert_eq!(snap.mission.workloads, 0);
        assert_eq!(snap.mission.degraded, 0);
        assert!(snap.workloads.is_empty());
    }

    #[test]
    fn snapshot_serialization_contains_locked_top_level_keys() {
        // Wire-schema v0.1 invariant — top-level keys must not be
        // renamed without coordinating with the web client (and the
        // v2 / Altara consumer once that lands). This regression-
        // guard breaks loudly if anyone accidentally renames a key.
        let snap = WireSnapshot::empty();
        let v: serde_json::Value = serde_json::to_value(&snap).expect("serialize");
        for key in [
            "tick",
            "server_time",
            "mission",
            "vitals",
            "workloads",
            "activity",
        ] {
            assert!(
                v.get(key).is_some(),
                "wire schema v0.1 missing top-level key {key:?}: {v}"
            );
        }
    }

    #[test]
    fn workload_status_strings_cover_all_four_variants() {
        // The dashboard reads `status` as one of four strings to
        // pick a dot color. Pin the mapping so adding a contract
        // variant on the Rust side doesn't silently render as
        // "unknown" on the web side.
        assert_eq!(
            workload_status_to_str(ux_contract::WorkloadStatus::Healthy),
            "healthy"
        );
        assert_eq!(
            workload_status_to_str(ux_contract::WorkloadStatus::Attention),
            "attention"
        );
        assert_eq!(
            workload_status_to_str(ux_contract::WorkloadStatus::Critical),
            "critical"
        );
        assert_eq!(
            workload_status_to_str(ux_contract::WorkloadStatus::Loading),
            "loading"
        );
    }

    /// v1.0.1 B-NEW-9 — when the platform layer surfaces a non-zero
    /// total, RSS megabytes get projected to a percentage so the web
    /// row can render "121M (0.4%)" alongside the absolute figure.
    #[test]
    fn rss_renders_as_percentage_when_total_known() {
        // 121 MB ÷ 32 GB ≈ 0.369 %.
        let pct = compute_ram_pct(121, Some(32 * 1024 * 1024 * 1024)).unwrap();
        assert!(
            (pct - 0.369).abs() < 0.01,
            "expected ~0.37%; got {pct}"
        );

        // 16 GB ÷ 32 GB = 50 %.
        let half = compute_ram_pct(16 * 1024, Some(32 * 1024 * 1024 * 1024)).unwrap();
        assert!((half - 50.0).abs() < 0.001, "expected 50%; got {half}");
    }

    /// v1.0.1 B-NEW-9 — no total ⇒ no percentage. The dashboard
    /// then renders the bare megabyte figure rather than a misleading
    /// "0.0%".
    #[test]
    fn rss_falls_back_to_absolute_when_total_unknown() {
        assert!(compute_ram_pct(121, None).is_none());
        assert!(compute_ram_pct(121, Some(0)).is_none());
    }

    #[test]
    fn exit_kind_mapping_pins_wire_strings() {
        // Wire-stability check: the strings on the right are part of
        // the locked schema. Don't rename without bumping the
        // protocol version.
        use crate::lifecycle::LifecycleSummary;
        use chrono::Utc;
        fn lc(signal: Option<i32>, exit_code: Option<i32>) -> LifecycleSummary {
            LifecycleSummary {
                pid: 1,
                name: "x".into(),
                category: None,
                model_name: None,
                spawn_time: Utc::now(),
                exit_time: Utc::now(),
                uptime_secs: 1,
                exit_code,
                signal,
                avg_cpu_pct: 0.0,
                peak_cpu_pct: 0.0,
                peak_rss_mb: 0,
                peak_vram_mb: 0,
                samples: 1,
            }
        }
        let clean = WireRunRecord::from_record(&RunRecord::from_summary(lc(None, Some(0))));
        assert_eq!(clean.exit_kind, "clean");
        let signal = WireRunRecord::from_record(&RunRecord::from_summary(lc(Some(15), None)));
        assert_eq!(signal.exit_kind, "signal");
        let segfault = WireRunRecord::from_record(&RunRecord::from_summary(lc(Some(11), None)));
        assert_eq!(segfault.exit_kind, "segfault");
        let crash = WireRunRecord::from_record(&RunRecord::from_summary(lc(None, Some(139))));
        assert_eq!(crash.exit_kind, "crash");
    }
}
