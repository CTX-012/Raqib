//! ux_contract — the single source of truth for edge_monitor's user-facing
//! behavior. Both the Linux and Windows binaries depend on this crate.
//! Editing this crate is the only way to change UX behavior.
//!
//! Contract version: 0.3
//! Locked: see UX_CONTRACT.md
//!
//! When you change anything in this file:
//! 1. Bump CONTRACT_VERSION
//! 2. Update UX_CONTRACT.md
//! 3. Re-run golden-image tests in both repos

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::all)]

/// Contract version. Bumped when the contract changes in any way.
pub const CONTRACT_VERSION: &str = "0.3.22";

// ============================================================================
// CAR-17 — kill_confirm card surface
// ============================================================================

pub mod kill_confirm_card;

// ============================================================================
// CAR-21 (v0.3.12) — ActivityState lifted from edge_monitor Phase 2
// ============================================================================

pub mod activity;

// ============================================================================
// CAR-22 (v0.3.13) — HostVitals host-level wire type (thermal first)
// ============================================================================

pub mod host_vitals;

// ============================================================================
// CAR-23 (v0.3.14) — Recommendation surface (Phase 3 observe-only firewall)
// ============================================================================

pub mod recommendation;

// ============================================================================
// CAR-D93 (v0.3.20) — History wire surface (PHASE 5 step 6 gate)
// ============================================================================
//
// Contract-side of the /api/history endpoint. Path constants,
// canonical wire field names, cap constants, and the documented
// shape decisions (two-endpoint split, ActivityKind reuse, VRAM
// honesty). Actual wire structs (with serde derives) live in the
// consumer per the crate's dependency-free stance.

pub mod history;

// ============================================================================
// CAR-D97 (v0.3.21) — TUI history-events browse overlay copy strings
// ============================================================================
//
// The TUI-side event archive browser (PHASE 5 step 9). Header +
// empty-state + reload-status templates. Scoped to events only —
// NO chart or sparkline strings live in this module (the SVG
// trajectory chart is web-only by decision; the TUI is a clean
// event browser).

/// Copy strings for the TUI history-events browse overlay
/// (CAR-D97 / DISPATCH 97 / PHASE 5 step 9). Consumers substitute
/// `{time}` / `{count}` verbatim; no per-locale variants — one
/// contract, one rendering.
pub mod history_events {
    /// Header shown at the top of the overlay. `{time}` substitutes
    /// with the snapshot's fetch time (locale-formatted HH:MM:SS by
    /// the consumer). Ends by naming the modal keys the operator
    /// needs — the panel IS the help for its own scope.
    pub const TITLE: &str =
        "History events (H) — j/k navigate, r reload, Esc close, snapshot @ {time}";
    /// Rendered in place of the event list when the archive is
    /// empty at snapshot time (no exits/kills/regressions have
    /// happened yet, or the runtime just started).
    pub const EMPTY: &str = "No events in the archive.";
    /// Transient status-footer template fired after `r` reloads
    /// the snapshot. `{count}` substitutes with the number of
    /// events now visible. Format matches the D74 `ALERTS_ACKNOWLEDGED`
    /// footer style — short, past-tense, count-forward.
    pub const RELOAD_TEMPLATE: &str = "History reloaded ({count} events).";
}

// ============================================================================
// §3 — Status dots (semantic states for workload rows)
// ============================================================================

/// Status of a workload, shown as a colored dot on the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkloadStatus {
    /// Green ●. All thresholds OK, throughput within ±10% of baseline.
    Healthy,
    /// Amber ⚠. Resource pressure or throughput regression.
    Attention,
    /// Red ✕. Critical: VRAM/KV ≥ 95%, governor armed, OOM detected.
    Critical,
    /// Gray ○. Less than 30s of telemetry, no baseline.
    Loading,
}

impl WorkloadStatus {
    /// Unicode symbol for this status. ASCII fallback in `symbol_ascii()`.
    pub const fn symbol(self) -> &'static str {
        match self {
            WorkloadStatus::Healthy => "●",
            WorkloadStatus::Attention => "⚠",
            WorkloadStatus::Critical => "✕",
            WorkloadStatus::Loading => "○",
        }
    }

    /// ASCII fallback when the terminal can't render Unicode block characters.
    pub const fn symbol_ascii(self) -> &'static str {
        match self {
            WorkloadStatus::Healthy => "*",
            WorkloadStatus::Attention => "!",
            WorkloadStatus::Critical => "X",
            WorkloadStatus::Loading => "o",
        }
    }
}

// ============================================================================
// Thresholds (drive WorkloadStatus computation)
// ============================================================================

/// Resource and performance thresholds. All percentages 0.0..=100.0.
pub mod thresholds {
    /// VRAM utilization that triggers Attention dot.
    pub const VRAM_ATTENTION_PCT: f64 = 85.0;
    /// VRAM utilization that triggers Critical dot.
    pub const VRAM_CRITICAL_PCT: f64 = 95.0;

    /// RAM utilization that triggers Attention.
    pub const RAM_ATTENTION_PCT: f64 = 90.0;
    /// RAM utilization that triggers Critical.
    pub const RAM_CRITICAL_PCT: f64 = 95.0;

    /// KV cache utilization that triggers Attention (LLM only).
    pub const KV_ATTENTION_PCT: f64 = 80.0;
    /// KV cache utilization that triggers Critical (LLM only).
    pub const KV_CRITICAL_PCT: f64 = 95.0;

    /// Throughput as fraction of baseline below which Attention fires.
    /// E.g. 0.80 means "current ≤ 80% of baseline → Attention".
    pub const THROUGHPUT_ATTENTION_RATIO: f64 = 0.80;

    /// Bar graph color shifts to Attention at this utilization.
    pub const BAR_ATTENTION_PCT: f64 = 85.0;
    /// Bar graph color shifts to Critical at this utilization.
    pub const BAR_CRITICAL_PCT: f64 = 95.0;

    /// Sustained-pressure window before alerts fire. Seconds.
    pub const ALERT_SUSTAIN_SECS: u64 = 5;

    /// Time before a workload has enough telemetry for a baseline. Seconds.
    pub const BASELINE_WARMUP_SECS: u64 = 30;

    /// Armed-kill window. Seconds.
    pub const KILL_ARM_WINDOW_SECS: u64 = 5;

    // ------------------------------------------------------------------
    // CAR-22 (v0.3.13) — Host-level thermal classification thresholds
    // ------------------------------------------------------------------
    //
    // Distinct from `BAR_ATTENTION_PCT` / `BAR_CRITICAL_PCT` despite the
    // matching numeric values: those are percentage thresholds for
    // memory / VRAM bars (unit: 0.0..=100.0 %), while these are
    // temperature thresholds for `host_vitals::ThermalZone::temp_celsius`
    // (unit: degrees Celsius). The contract keeps them as separate
    // constants so a future tweak to one does not silently drift the
    // other.

    /// Per-zone temperature at or above which a thermal zone enters
    /// the "amber" severity bucket. Consumers classify at render
    /// time; see `host_vitals` module docs and the reference
    /// classifier in its tests.
    pub const THERMAL_AMBER_C: f64 = 85.0;

    /// Per-zone temperature at or above which a thermal zone enters
    /// the "red" severity bucket. Must be strictly greater than
    /// [`THERMAL_AMBER_C`]; enforced by a compile-time const-assert
    /// at module scope in `lib.rs`.
    pub const THERMAL_RED_C: f64 = 95.0;
}

// ============================================================================
// UX-CAR-002 — Power / energy defaults
// ============================================================================

/// Power and energy defaults consumed by `EnergyAccumulator` and the
/// `[power]` config block. Currently a single rate constant; future
/// power-related shared defaults belong here.
pub mod power {
    /// Default electricity rate when user config does not override.
    /// Anchored to the US national average residential rate
    /// (approximately $0.16/kWh circa 2025).
    ///
    /// Users who want different rates set them in their config TOML:
    ///
    /// ```toml
    /// [power]
    /// kwh_rate_usd = 0.20
    /// ```
    ///
    /// This constant is the fallback for unconfigured deployments.
    /// Bumping the default value requires a CONTRACT_VERSION bump
    /// (consumers may have anchored cost reports to the prior value).
    pub const DEFAULT_KWH_RATE_USD: f64 = 0.16;
}

// ============================================================================
// CAR-19c — Activity-feed caps (B-NEW-10, v0.3.10)
// ============================================================================

/// Render-side caps for the activity feed. Pre-v1.0.1 the same feed was
/// truncated to three different limits in three different places — the
/// TUI overlay capped at one number, the wire format at another, the
/// web UI at a third. Inspector report #3 B-NEW-10 surfaced the drift;
/// v1.0.1 unified the consumers against contract constants but the
/// constants themselves only land here (v0.3.10).
///
/// Invariants enforced by const-asserts and tests below:
///   * `ACTIVITY_FEED_TUI_MAX <= ACTIVITY_FEED_WIRE_MAX`
///   * `ACTIVITY_FEED_WEB_MAX <= ACTIVITY_FEED_WIRE_MAX`
///
/// Rationale: the wire-format cap bounds what's shipped between
/// processes (Linux runtime → TUI / web), so per-render-target caps
/// must not exceed it — otherwise the renderer would request more
/// entries than the wire can ever deliver and the deficit would be
/// silent. TUI vs Web caps are independent (different screen budgets);
/// neither is required to be larger than the other.
pub mod limits {
    /// Maximum activity-feed entries rendered in the TUI overlay.
    /// Bounded by the TUI's compact §1 region layout; entries beyond
    /// this cap collapse into a "+N more" indicator.
    pub const ACTIVITY_FEED_TUI_MAX: usize = 5;

    /// Maximum activity-feed entries the wire format carries between
    /// the Linux runtime and any UI consumer (TUI or web). Acts as the
    /// upper bound on the other two caps.
    pub const ACTIVITY_FEED_WIRE_MAX: usize = 50;

    /// Maximum activity-feed entries rendered in the web UI's
    /// Activity panel. Larger than the TUI cap because the web panel
    /// has a vertical scroll budget the TUI overlay lacks; smaller
    /// than the wire cap because the panel paginates the rest.
    pub const ACTIVITY_FEED_WEB_MAX: usize = 12;

    // ------------------------------------------------------------------
    // CAR-23 (v0.3.14) — Recommendation render-side caps
    // ------------------------------------------------------------------
    //
    // Per the Inspector v1.2.0 design doc §2. Mirrors
    // `ALERT_MAX_VISIBLE` so recs and alerts share a visual envelope.
    // A compile-time const-assert in `lib.rs` enforces
    // `REC_MAX_VISIBLE <= ALERT_MAX_VISIBLE` — recs are a render-time
    // projection of visible alerts and cannot outnumber the alerts
    // they ride on.

    /// Maximum number of recommendations rendered simultaneously
    /// in the rec section. Excess recs collapse into a "+N more"
    /// indicator the way alerts do.
    pub const REC_MAX_VISIBLE: usize = 3;

    /// Maximum ranked targets within a single recommendation.
    /// Caps the `targets:` list on
    /// [`crate::recommendation::Recommendation`]; relevant for
    /// system-scope recs that rank top-N contributors (RAM
    /// pressure). VRAM/KV recs have a single target; thermal has
    /// none.
    pub const REC_TARGETS_MAX: usize = 3;
}

// ============================================================================
// §4 — Alerts (sticky banners above the header)
// ============================================================================

/// Identity of an alert. Used for de-duplication and acknowledgment tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertId {
    /// VRAM_PRESSURE: VRAM ≥ 85% sustained 5s.
    VramPressure,
    /// RAM_PRESSURE: RAM ≥ 90% sustained 5s.
    RamPressure,
    /// KV_PRESSURE: KV cache ≥ 85% sustained 5s.
    KvPressure,
    /// GOVERNOR_ARMED: a manual kill is armed against a workload.
    GovernorArmed,
    /// OOM_DETECTED: kernel OOM kill in last 30s.
    OomDetected,
    /// WORKLOAD_EXITED: a workload exited with non-zero, OOM, or governor kill.
    /// Clean exits (code 0) do NOT raise this alert.
    WorkloadExited,
    /// THERMAL_PRESSURE: a host thermal zone is at or above
    /// [`thresholds::THERMAL_AMBER_C`]. System-scope alert (no
    /// per-PID attribution — host temperature is whole-die /
    /// zone-level on Linux). Drives the v1.2.0 thermal
    /// recommendation projection
    /// (`SuggestedAction::ConsiderReduceLoad`, empty targets).
    ///
    /// CAR-24 (v0.3.15) — closes Inspector v1.2.0 design §10 Q1.
    /// Single AlertId rather than separate `ThermalAmber` /
    /// `ThermalRed` variants: matches the existing
    /// VramPressure / KvPressure / RamPressure pattern where one
    /// AlertId fires at the attention threshold and persists,
    /// with the consumer computing the render-time severity tier
    /// from the current value vs [`thresholds::THERMAL_AMBER_C`]
    /// and [`thresholds::THERMAL_RED_C`].
    ThermalPressure,
}

/// Maximum alerts visible simultaneously before "+N more".
pub const ALERT_MAX_VISIBLE: usize = 3;

// ============================================================================
// §6 — Keymap
// ============================================================================

/// Every action the TUI can dispatch in response to input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Quit the application (with confirm if kill armed).
    Quit,
    /// Toggle the help overlay.
    ToggleHelp,
    /// Move workload selection up.
    SelectUp,
    /// Move workload selection down.
    SelectDown,
    /// Arm a kill on the focused workload (or confirm if already armed).
    /// CAR-14: arm fires on `k`; confirm fires on `Enter` once armed
    /// (not a repeated `k` — see [`kill_keybinding`]).
    KillOrConfirm,
    /// Open the detail card (live or post-mortem based on row state).
    OpenDetail,
    /// Toggle the history overlay.
    ToggleHistory,
    /// Acknowledge all currently visible alerts.
    AcknowledgeAlerts,
    /// Cycle the Top processes panel sort: RAM → CPU → VRAM.
    CycleTopSort,
    /// Esc cascade — see UX_CONTRACT.md §6 for resolution order.
    EscapeCascade,
    /// Toggle activity-browse mode (j/k navigate, Enter expand).
    /// See UX_CONTRACT §1 r6 / CAR-D75.
    ToggleActivityBrowse,
    /// v0.3.21 / CAR-D97 / DISPATCH 97 — toggle the TUI history-events
    /// browse mode. Opens/closes an in-place overlay listing the History
    /// event archive (exits/kills/regressions, cap
    /// [`history::EVENT_ARCHIVE_MAX`]) newest-first. SCOPED to events —
    /// no trajectory charts in the TUI (SVG earns time-series; terminals
    /// don't). Web-side equivalent is HistoryPage (D95).
    ///
    /// Modal capture identical to [`Action::ToggleActivityBrowse`]:
    /// while active, j/k navigate a composite-key cursor and `Esc`
    /// closes. Bound to capital `H` — coexists with lowercase `h`
    /// (which is [`Action::ToggleHistory`], the RunStore per-model
    /// overlay). DIFFERENT SURFACES; both preserved.
    ///
    /// Reload key `r` inside the overlay is handled locally in the
    /// modal input scope (matching the RunStore overlay's `h`/`q`
    /// precedent — no distinct Action round-trip).
    ToggleHistoryEvents,
}

// ============================================================================
// §7 — Copy strings (every user-visible string in the TUI)
// ============================================================================

/// Status footer messages. `{placeholder}` substituted at render time.
///
/// CAR-19b (v0.3.10) removed four orphaned constants — Sprint 5 had
/// hard-deleted the Grafana integration but the footer/dashboard
/// templates lingered through v0.3.9. Removed surfaces:
/// `DASHBOARD_OPENED`, `DASHBOARD_FAILED`, `GRAFANA_UNREACHABLE`,
/// `KILL_DRY_RUN`. Sprint 1's dry-run feature was also removed
/// upstream, so `KILL_DRY_RUN_PREFIX` is gone with the dry-run
/// footer message itself. `KILL_ALLOWLIST_PREFIX` remains —
/// allowlist override is a separate feature.
pub mod status {
    /// Footer message when the user invokes a workload-targeted action with no row focused.
    pub const NO_WORKLOAD_FOCUSED: &str = "No AI workload focused";
    /// Footer message after first 'k' press.
    pub const KILL_ARMED: &str =
        "Armed kill on {name} (PID {pid}) — press Enter within {secs}s";
    /// Footer message after kill signal sent.
    pub const KILL_SENT: &str = "Sent SIGTERM to {name} (PID {pid})";
    /// Footer message after the force-SIGKILL escalation is sent (CAR-D82,
    /// v0.3.19). The escalated-path counterpart to [`KILL_SENT`]: when a
    /// workload survives the SIGTERM grace window and the operator confirms
    /// force-kill in the kill_confirm card's Waiting state, the consumer
    /// sends SIGKILL and this footer reports it. Same `{name}` / `{pid}`
    /// render-time placeholder convention as [`KILL_SENT`]. Distinct from
    /// `KILL_SENT` so the footer honestly reflects WHICH signal fired —
    /// SIGKILL is the uncatchable escalation, not the graceful first signal.
    pub const KILL_FORCE_SENT: &str = "Sent SIGKILL to {name} (PID {pid})";
    /// Footer message after Esc disarms a kill.
    pub const KILL_DISARMED: &str = "Kill disarmed";
    /// Footer message when kill is blocked by allowlist.
    pub const GOVERNOR_BLOCKED: &str = "Cannot kill {name}: protected by allowlist";
    /// Footer message after 'a' acknowledges alerts.
    pub const ALERTS_ACKNOWLEDGED: &str = "Acknowledged {n} alerts";
    /// Footer message when Enter pressed on a system process row.
    pub const NO_DETAIL_FOR_SYSTEM: &str = "Detail not available for system processes";
    /// Footer message after 't' cycles top-processes sort.
    pub const TOP_SORT_CHANGED: &str = "Top processes sorted by {dimension}";

    // ------------------------------------------------------------------
    // CAR-5 — Default footer keymap
    // ------------------------------------------------------------------

    /// Default footer keymap line, rendered in the footer when no
    /// transient status (kill countdown, action confirmation, etc.) is
    /// active. Maps to the §1 region 7 layout. Transient states override
    /// it temporarily, then it returns.
    ///
    /// CAR-19b (v0.3.10) dropped the `g graph` token: Sprint 5 hard-
    /// deleted the Grafana integration and `g` is now unbound, so the
    /// hint was advertising a key that does nothing.
    pub const FOOTER_KEYMAP: &str =
        "Enter detail · k kill · h history · ? help · q quit";

    // ------------------------------------------------------------------
    // CAR-4 — Armed-kill countdown (footer side of the alert/footer split)
    // ------------------------------------------------------------------
    //
    // Design note: the countdown lives in the FOOTER, not the armed-kill
    // ALERT (`alerts::GOVERNOR_ARMED`), by design. Alerts are sticky and
    // don't tick. Putting a per-second-rerendering countdown into the
    // alert region would require alerts to support live-updating
    // templates, complicating the alert state machine for one feature.
    // The split — static alert text + dynamic footer countdown — keeps
    // each region's contract simple. Don't move the countdown into the
    // alert; extend this comment if the rationale ever changes.

    /// Countdown rendered in the footer when a kill is armed. `{secs}` is
    /// the integer seconds remaining (5, 4, 3, 2, 1). When the timer
    /// expires, the alert auto-disarms and the countdown disappears.
    pub const KILL_COUNTDOWN: &str = "{secs}s to confirm or Esc to cancel";

    /// Prepended to `KILL_COUNTDOWN` when an allowlist override is in
    /// effect for the focused workload.
    pub const KILL_ALLOWLIST_PREFIX: &str = "Allowlist override — ";

    // ------------------------------------------------------------------
    // CAR-7 — Workload-row metric placeholders
    // CAR-19a — RUNNING_ACTIVELY companion (v0.3.10)
    // ------------------------------------------------------------------

    /// Rendered in the workload row's primary-metric position when the
    /// workload has less than `thresholds::BASELINE_WARMUP_SECS` of
    /// telemetry — i.e. when `WorkloadStatus::Loading` is the
    /// classification. Replaces the type-specific value (tok/s, fps,
    /// emb/s, Hz) for the duration of the warm-up window. See §2.
    pub const COLD_LOADING: &str = "cold-loading";

    /// Rendered in the workload row's primary-metric position when the
    /// workload is past the warm-up window but the per-category
    /// telemetry sampler has nothing concrete to show (no token rate
    /// observed yet for an LLM, no frames seen yet for Vision, no
    /// topic rate yet for ROS2, etc.). Falls back here instead of
    /// rendering a zero or stale value.
    ///
    /// CAR-19a (v0.3.10): the literal `"running actively"` was
    /// duplicated between `src/ui/panels/workloads.rs` (Rust TUI) and
    /// `web/src/components/WorkloadRow.svelte` (web UI). Inspector
    /// surfaced the drift risk in B-NEW-5; lifting the string here
    /// gives both consumers a single source of truth. Distinct from
    /// `COLD_LOADING`: cold-loading covers the warm-up window;
    /// running-actively covers post-warm-up but no metric yet.
    pub const RUNNING_ACTIVELY: &str = "running actively";

    /// Rendered in the workload row's primary-metric position for
    /// Agent-category workloads (SaaS-LLM developer-assistant CLIs:
    /// claude-code, cursor, aider, continue, and similar) instead of
    /// [`RUNNING_ACTIVELY`].
    ///
    /// Rationale (v1.0.1 B-NEW-4, filed as a CAR in `edge_monitor`'s
    /// CHANGELOG and BACKLOG): "running actively" overclaims for
    /// Agent rows. These processes proxy to a remote LLM and
    /// edge_monitor measures none of the per-request rate locally —
    /// the only signal it can honestly produce is "the process
    /// exists on this host and is in our annotated set." `"alive"`
    /// is that honest minimum: process-existence-only, no further
    /// claim about request rate, model, or activity.
    ///
    /// CAR-20 (v0.3.11): the literal `"alive"` was duplicated
    /// between `src/ui/panels/workloads.rs::AGENT_ALIVE` (Rust TUI)
    /// and `web/src/components/WorkloadRow.svelte` (web UI). The
    /// consumer filed this in v1.0.1 and re-filed it in BACKLOG
    /// after v0.3.10 did not pick it up; lifting it here finally
    /// gives both consumers a single source of truth and unblocks
    /// Windows parity. Pairs with [`RUNNING_ACTIVELY`]:
    /// running-actively for non-Agent post-warm-up rows,
    /// agent-alive for Agent post-warm-up rows.
    pub const AGENT_ALIVE: &str = "alive";

    /// Rendered in any VRAM field for which edge_monitor has no
    /// measurement (no NVML handle, non-GPU host, or sampler not yet
    /// primed). The renderer MUST NOT substitute `"0 MB"` for an
    /// unmeasured field — a literal zero reads as "measured and empty",
    /// which overclaims. This honest placeholder keeps unmeasured
    /// distinct from genuinely-zero. CAR-D75 / UX_CONTRACT §1 r6.
    pub const VRAM_UNMEASURED: &str = "no measurements";
}

/// Empty-state strings. Shown inside panels when no data is available.
pub mod empty {
    /// Workloads panel empty.
    pub const WORKLOADS: &str = "No AI workloads detected. Start one to begin monitoring.";
    /// Activity panel empty.
    pub const ACTIVITY: &str = "No recent activity.";
    /// History overlay empty.
    pub const HISTORY: &str = "No history yet. Completed runs will appear here.";
}

/// Alert message templates. `{placeholder}` substituted at render time.
pub mod alerts {
    /// Template for VRAM pressure alert.
    pub const VRAM_PRESSURE: &str = "VRAM at {pct}% — {workload} (PID {pid}) approaching limit";
    /// Template for RAM pressure alert.
    pub const RAM_PRESSURE: &str = "RAM at {pct}% — system approaching limit";
    /// Template for KV cache pressure alert.
    pub const KV_PRESSURE: &str = "KV cache at {pct}% — {workload} (PID {pid}) may stall";
    /// Template for governor-armed alert.
    pub const GOVERNOR_ARMED: &str =
        "Kill armed on {workload} (PID {pid}) — Enter confirms, Esc cancels";
    /// Template for OOM-detected alert.
    pub const OOM_DETECTED: &str =
        "OOM kill detected — {workload} (PID {pid}) terminated by kernel";
    /// Template for non-clean workload-exit alert.
    pub const WORKLOAD_EXITED: &str =
        "{workload} exited with {reason} — press Enter for post-mortem";
    /// Template for thermal-pressure alert (CAR-24, v0.3.15).
    /// System-scope: no `{pid}` / `{workload}` placeholder
    /// because host thermal is whole-die / zone-level and
    /// cannot be sensibly attributed to one PID (Inspector
    /// v1.2.0 design §1b option (a) — honest about the
    /// host-level scope, no causal-attribution guess). The
    /// `{temp_c}` placeholder is the offending zone's reading
    /// in degrees Celsius.
    pub const THERMAL_PRESSURE: &str =
        "Thermal at {temp_c}°C — system thermal pressure";
}

/// Confirmation prompt strings.
pub mod confirm {
    /// Prompt when user attempts to quit while a kill is armed.
    pub const QUIT_KILL_PENDING: &str = "Kill armed on {workload}. Quit anyway? (y/N)";
}

/// Error messages shown when the TUI cannot render normally.
pub mod errors {
    /// Shown when terminal is below the 80×24 minimum.
    pub const TERMINAL_TOO_SMALL: &str =
        "raqib needs at least 80×24 terminal.\nCurrent size: {w}×{h}. Resize and press any key.";
}

// ============================================================================
// CAR-1 — Help overlay copy (rendered when user presses `?`)
// ============================================================================

/// Copy strings for the help overlay. Rendered as a static modal listing
/// every keybinding in `Action`. Each `KEY_*` constant is a fixed-width
/// `"{key}  {description}"` line; the renderer aligns columns by the
/// constants' shape, not by reformatting at runtime.
pub mod help {
    /// Title of the help overlay.
    pub const TITLE: &str = "raqib — keyboard reference";

    /// Section header: navigation-only keys.
    pub const SECTION_NAVIGATION: &str = "Navigation";
    /// Section header: action keys (kill, ack, etc.).
    pub const SECTION_ACTIONS: &str = "Actions";
    /// Section header: overlay-toggle keys (help, history, detail).
    pub const SECTION_OVERLAYS: &str = "Overlays";

    /// `q` — quit.
    pub const KEY_QUIT: &str = "q       Quit";
    /// `?` — toggle help overlay.
    pub const KEY_HELP: &str = "?       Toggle this help overlay";
    /// `j` — select next workload.
    pub const KEY_SELECT_DOWN: &str = "j       Select next workload";
    /// `k` / uppercase / Up — select previous workload. Documents the
    /// L2a-decided extension (uppercase K and Up arrow both bind to
    /// SelectUp; lowercase k separately binds to KillOrConfirm in the
    /// workloads pane).
    pub const KEY_SELECT_UP: &str =
        "k / K   Select previous workload (uppercase or Up)";
    /// `k` (workloads-pane) — arm kill on focused workload (Enter to confirm).
    pub const KEY_KILL: &str =
        "k       Arm kill on focused workload (Enter to confirm)";
    /// `Enter` — open detail card (live for running, post-mortem for exited).
    pub const KEY_DETAIL: &str = "Enter   Open detail card (live or post-mortem)";
    /// `h` — toggle history overlay.
    pub const KEY_HISTORY: &str = "h       Toggle history overlay";
    /// `a` — acknowledge all visible alerts.
    pub const KEY_ACK_ALERTS: &str = "a       Acknowledge all visible alerts";
    /// `t` — cycle Top processes panel sort: RAM → CPU → VRAM.
    pub const KEY_TOP_SORT: &str = "t       Cycle top processes sort: RAM/CPU/VRAM";
    /// `Esc` — cascade dismiss (overlay → kill → alerts → quit, see §6).
    pub const KEY_ESC: &str = "Esc     Cascade dismiss (overlay/kill/alerts/quit)";
    /// `A` — toggle activity-browse mode (CAR-D75, UX_CONTRACT §1 r6).
    pub const KEY_ACTIVITY_BROWSE: &str =
        "A       Toggle activity browse mode (j/k navigate, Enter expand)";
    /// `H` — toggle the history-events browse overlay (CAR-D97,
    /// PHASE 5 step 9). SEPARATE from lowercase `h` (which is
    /// [`super::Action::ToggleHistory`], the RunStore per-model overlay).
    pub const KEY_HISTORY_EVENTS: &str =
        "H       Browse history event archive (j/k navigate, r reload, Esc close)";

    /// Footer hint at the bottom of the help overlay.
    pub const FOOTER: &str = "Press ? or Esc to close";
}

// ============================================================================
// CAR-2 — Post-mortem card field labels (§5)
// ============================================================================

/// Field labels rendered in the post-mortem card. The card layout itself
/// is governed by `sizing::CARD_WIDTH` and the `§5` schema.
pub mod postmortem_labels {
    /// "Cause:" — exit-reason line.
    pub const CAUSE: &str = "Cause:";
    /// "Runtime:" — wall-clock duration of the run.
    pub const RUNTIME: &str = "Runtime:";
    /// "Throughput:" — followed by current value and baseline comparison.
    pub const THROUGHPUT: &str = "Throughput:";
    /// "Peak RAM:" — peak resident set during the run.
    pub const PEAK_RAM: &str = "Peak RAM:";
    /// "Peak VRAM:" — peak GPU memory during the run (GPU workloads only).
    pub const PEAK_VRAM: &str = "Peak VRAM:";
    /// "KV cache:" — KV cache utilization at exit (LLM only).
    pub const KV_CACHE: &str = "KV cache:";
    /// "Energy:" — total joules consumed during the run.
    pub const ENERGY: &str = "Energy:";
    /// "Last stderr:" — section header for transient stderr (only shown
    /// if card is opened within 30s of exit; see §5).
    pub const LAST_STDERR: &str = "Last stderr:";
    /// Footer hint at the bottom of the post-mortem card.
    pub const FOOTER: &str = "Esc dismiss · h history · g graph";
    /// "Kill action:" — the kill signal/action issued against the
    /// workload, shown when the run ended via a user-issued kill
    /// (CAR-D75).
    pub const KILL_ACTION: &str = "Kill action:";
    /// "Kill result:" — the observed outcome of the kill action
    /// (CAR-D75).
    pub const KILL_RESULT: &str = "Kill result:";
}

// ============================================================================
// CAR-6 — History overlay copy
// ============================================================================

/// Copy strings for the history overlay (toggled by `h`).
pub mod history_labels {
    /// Title of the history overlay.
    pub const TITLE: &str = "Run history";

    /// Subheader showing run count. `{n}` is total runs visible.
    pub const HEADER_RUN_COUNT: &str = "{n} runs · most recent first";

    /// Fixed-width column header line — must match per-row formatting in
    /// the renderer (`src/ui/panels/history_overlay.rs` on Linux,
    /// equivalent on Windows). No tabs; alignment via spaces.
    pub const COLUMN_HEADER: &str =
        "#  When        Dur    AvgCPU  PeakRSS  PeakVRAM  Exit";

    /// Badge appended to the Exit column when KV cache was at saturation
    /// at run-end.
    pub const KV_SATURATION_BADGE: &str = "KV!";

    /// Footer hint at the bottom of the history overlay.
    pub const FOOTER: &str = "Esc / h close · Enter open post-mortem · q quit";
}

// ============================================================================
// CAR-8 — Workload-category section headers (§1 region 4)
// CAR-18 — Agent category added (v0.3.9)
// ============================================================================

/// Section headers rendered between workload-category groups in the
/// Workloads panel. The `── X ──` format is canonical.
///
/// Exposed as named constants rather than as a function over a
/// contract-owned `WorkloadCategory` enum: the enum lives in the
/// consumer's classifier code, and lifting it would force a workspace-
/// wide rename without changing the rendered output. Consumers map
/// their local enum to the constant at the call site:
///
/// ```ignore
/// let header = match category {
///     LocalWorkloadCategory::LLM        => GROUP_HEADER_LLM,
///     LocalWorkloadCategory::Vision     => GROUP_HEADER_VISION,
///     LocalWorkloadCategory::ROS2       => GROUP_HEADER_ROS2,
///     LocalWorkloadCategory::Embeddings => GROUP_HEADER_EMBEDDINGS,
///     LocalWorkloadCategory::Agent      => GROUP_HEADER_AGENT,
///     LocalWorkloadCategory::Unknown    => GROUP_HEADER_UNKNOWN,
/// };
/// ```
///
/// CAR-18 (v0.3.9): `GROUP_HEADER_AGENT` covers SaaS-LLM developer-
/// assistant CLIs (Claude Code, Cursor, Aider, Continue, and similar).
/// These processes USE LLMs but are not LLM inference servers — Sprint 6
/// smoke testing surfaced that classifying them as LLM mixed
/// agent-style consumers with raw inference servers like `ollama`,
/// hiding the distinction users care about on the dashboard.
/// Additive: the LLM header is unchanged; pre-CAR-18 consumers that
/// keep classifying claude/cursor/aider as LLM continue to work and
/// simply render under the LLM section as before.
pub mod workload_category {
    /// Header rendered above the LLM section of the Workloads panel.
    /// CAR-18 note: LLM remains the inference-server category (ollama,
    /// vllm, llama.cpp). Developer-assistant SaaS-LLM CLIs now render
    /// under `GROUP_HEADER_AGENT` instead.
    pub const GROUP_HEADER_LLM: &str = "── LLM ──";
    /// Header rendered above the Vision section.
    pub const GROUP_HEADER_VISION: &str = "── Vision ──";
    /// Header rendered above the ROS2 section.
    pub const GROUP_HEADER_ROS2: &str = "── ROS2 ──";
    /// Header rendered above the Embeddings section.
    pub const GROUP_HEADER_EMBEDDINGS: &str = "── Embeddings ──";
    /// Header rendered above the Agent section — developer-assistant
    /// SaaS-LLM CLIs that USE a remote LLM (Claude Code, Cursor, Aider,
    /// Continue, and similar). Distinct from `GROUP_HEADER_LLM`, which
    /// covers local inference servers. Added in CAR-18 (v0.3.9).
    pub const GROUP_HEADER_AGENT: &str = "── Agent ──";
    /// Header rendered above the Unknown / unrecognized-AI section.
    pub const GROUP_HEADER_UNKNOWN: &str = "── Unknown ──";
}

// ============================================================================
// CAR-9 — Per-category degraded-row expansion templates (§2)
// ============================================================================

/// Templates for the second indented line shown beneath an Attention or
/// Critical workload row. The per-category schemas are locked in
/// DESIGN_HANDOFF.md §2 ("Degraded workload, expanded line"). Surfaced
/// by L12 (Linux) — the v1.0 implementation in
/// `src/ui/panels/workloads.rs::degraded_line()` currently emits a
/// content-light `·`-joined trigger list because none of the per-
/// category telemetry fields (queue depth, p99, live baseline, ±delta)
/// are tracked yet. Adding the templates here lets each impl swap to
/// the contract-locked format once those fields are wired.
///
/// Placeholders follow the same `{name}` convention as `alerts::*` and
/// `status::*` — substituted by the renderer at the call site. ROS2's
/// template is intentionally empty for v1.0; §2 defers its schema to
/// v1.1+. Consumers should skip rendering the expansion line entirely
/// when the template is empty rather than emitting a blank row.
pub mod degraded_line {
    /// LLM: KV cache pressure + queue depth + tail latency + baseline +
    /// signed delta. `{delta_pct}` is signed (e.g. `-45`); the renderer
    /// is expected to format with sign so the line reads `... · -45%`.
    pub const LLM: &str =
        "KV {kv_pct}% · queue {queue} · p99 {p99_ms}ms · baseline {baseline_tok_s} tok/s · {delta_pct}%";

    /// Vision: VRAM pressure + pipeline phase + baseline fps + signed
    /// delta. `{phase}` is a short free-text label (e.g. `decoding`,
    /// `inference`, `postproc`) the sampler attaches per frame.
    pub const VISION: &str =
        "VRAM {vram_pct}% · {phase} · baseline {baseline_fps} fps · {delta_pct}%";

    /// Embeddings: batch size + tail latency + baseline emb/s + signed
    /// delta.
    pub const EMBEDDINGS: &str =
        "batch {batch} · p99 {p99_ms}ms · baseline {baseline_emb_s} emb/s · {delta_pct}%";

    /// ROS2: empty for v1.0. §2 lists a schema
    /// (`topics {n} · queue {n} · baseline {Hz} · {±delta}%`) but marks
    /// it `(v1.1+)`. Consumers should skip the indented line entirely
    /// rather than render an empty row.
    pub const ROS2: &str = "";

    /// Unknown: single fixed message — no per-category metrics exist
    /// for unrecognised AI processes.
    pub const UNKNOWN: &str = "(unrecognized AI workload — no metrics)";
}

// ============================================================================
// CAR-11 — Top processes panel surface (§1 region 5)
// ============================================================================

/// Copy strings for the Top processes panel rendered in §1 region 5.
/// L13 (Linux) renders the panel title locally as
/// `"Top processes (by RAM)"`; this module exposes the prefix + sort-
/// dimension labels so the title and the `status::TOP_SORT_CHANGED`
/// footer message stay in lock-step when 't' cycles the sort.
///
/// Composition: the final panel title is
/// `"{PANEL_TITLE_PREFIX} (by {SORT_BY_*})"`. The same `SORT_BY_*`
/// constant substitutes into `status::TOP_SORT_CHANGED`'s
/// `{dimension}` placeholder so the footer message after a sort cycle
/// matches the panel header verbatim.
///
/// §1 region 5's prose currently says the panel "Filters … processes
/// already in Workloads", but the example in the same region shows
/// `ollama` (an AI workload) in Top processes. L13 implements per the
/// example (un-filtered). The prose update is a coordinated handoff
/// for DESIGN_HANDOFF.md in the LinuxImpl repo; this module does not
/// encode either interpretation — it only names the surface.
pub mod top_processes {
    /// Panel-title prefix, before the parenthesised sort dimension.
    pub const PANEL_TITLE_PREFIX: &str = "Top processes";

    /// Label for the RAM (resident set) sort dimension. Default sort.
    pub const SORT_BY_RAM: &str = "RAM";
    /// Label for the CPU% sort dimension.
    pub const SORT_BY_CPU: &str = "CPU";
    /// Label for the VRAM sort dimension (GPU workloads only —
    /// non-GPU processes sort to the bottom).
    pub const SORT_BY_VRAM: &str = "VRAM";
}

// ============================================================================
// CAR-13 — Mission-line template (§0 header)
// ============================================================================

/// Mission line rendered as the top-of-screen header row (§0). Surfaced
/// after Linux L25 (`f15a5f7`) and Windows W46 (`eb9f5a0`) both shipped
/// the same literal header text in parallel and independently flagged
/// the absence of a contract template as a drift risk. Lifting the
/// literal here closes the gap before the two consumers drift.
///
/// Composition: the renderer substitutes `{n}` (total workloads) and
/// `{m}` (degraded count, i.e. workloads in `Attention` or `Critical`)
/// into [`mission::TEMPLATE`] at render time, using the same
/// `{name}`-placeholder convention as `alerts::*`, `status::*`, and
/// `degraded_line::*`.
///
/// Plural handling is intentionally out of scope for v1.0: the template
/// reads `"{n} workloads"` regardless of whether `n` is 1. Pluralisation
/// is an i18n concern and is deferred to a later contract version;
/// consumers should not branch locally on `n == 1` to render
/// `"1 workload"` (singular) — that would diverge from the contract
/// across L25 / W46. If pluralisation lands, it lands here.
pub mod mission {
    /// Mission-line template for the top header row (§0). Substitutes
    /// `{n}` (total workloads) and `{m}` (degraded count — workloads in
    /// `Attention` or `Critical`).
    ///
    /// The literal mission-line shipped in Linux L25 (`f15a5f7`) and
    /// Windows W46 (`eb9f5a0`) is reproduced here verbatim; the
    /// `mission_template_matches_l25_w46_shipped_string` test in the
    /// inline `mod tests` locks the string so any future edit to this
    /// constant requires a coordinated update to both consumers.
    pub const TEMPLATE: &str =
        "raqib · {n} workloads · {m} degraded · press ? for help";
}

// ============================================================================
// CAR-14 — Kill keybinding (k-then-Enter, replacing double-k)
// ============================================================================

/// Keybinding for the armed-kill workflow. Replaces the prior double-k
/// pattern (press `k` to arm, press `k` again to confirm) with `k`-then-
/// `Enter` after user testing on live `cargo run` builds — on both Linux
/// and Windows — surfaced that the double-k confirmation was unreliable
/// in practice (ambiguity between "did I press k once or twice?" makes
/// the confirmation step error-prone).
///
/// State-driven dispatch is the consumer's responsibility:
///   * `k` in normal state fires `Action::KillOrConfirm` (arms).
///   * `Enter` in the armed state fires `Action::KillOrConfirm` (confirms).
///     In normal state, `Enter` still opens the detail card per
///     `help::KEY_DETAIL` — consumers must branch on armed-or-not.
///   * `Esc` in the armed state cancels, consistent with the global
///     Esc cascade (L24).
///
/// The visible footer/alert strings (`status::KILL_ARMED`,
/// `alerts::GOVERNOR_ARMED`, `help::KEY_KILL`) were updated in this
/// same CAR-14 to match the new semantics — search this file for
/// `Enter` to confirm the cross-string consistency.
pub mod kill_keybinding {
    /// First press arms the kill action on the currently selected
    /// workload. Visual: armed-kill banner appears (theme.background on
    /// theme.critical per L21 styling).
    pub const ARM_KEY: char = 'k';

    /// Second press confirms and fires the kill. Using Enter
    /// (`KeyCode::Enter`) rather than a repeat of `k` to avoid the
    /// double-tap ambiguity that surfaced in user testing.
    pub const CONFIRM_KEY_NAME: &str = "Enter";

    /// Esc at any point in the armed state cancels (consistent with the
    /// global Esc cascade — L24).
    pub const CANCEL_KEY_NAME: &str = "Esc";

    /// Banner text shown during armed state.
    pub const ARMED_BANNER: &str = "Press Enter to confirm kill · Esc cancel";
}

// ============================================================================
// §13 — Themes
// ============================================================================

/// One of the three v1.0 themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeName {
    /// Tokyo Night-inspired dark palette. Default.
    Dark,
    /// Cream-background light palette (not pure white).
    Light,
    /// Pure-black-on-white with bright primaries; WCAG AAA.
    HighContrast,
}

/// Hex color values for one theme. Strings so they can be parsed at render time
/// by the platform-specific TUI layer (ratatui::style::Color).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Theme identity.
    pub name: ThemeName,
    /// Main background color hex.
    pub background: &'static str,
    /// Slightly lighter panel-surface background.
    pub background_raised: &'static str,
    /// Primary text color.
    pub foreground: &'static str,
    /// Secondary / muted text color.
    pub muted: &'static str,
    /// Selection highlight, title bar, key hints.
    pub accent: &'static str,
    /// Healthy dot color.
    pub healthy: &'static str,
    /// Attention dot / amber alert color.
    pub attention: &'static str,
    /// Critical dot / red alert color.
    pub critical: &'static str,
}

impl Theme {
    /// Returns the theme matching the given name.
    pub const fn for_name(name: ThemeName) -> Self {
        match name {
            ThemeName::Dark => DARK,
            ThemeName::Light => LIGHT,
            ThemeName::HighContrast => HIGH_CONTRAST,
        }
    }
}

/// Tokyo Night-inspired dark theme. WCAG AA verified.
pub const DARK: Theme = Theme {
    name: ThemeName::Dark,
    background: "#1a1b26",
    background_raised: "#24283b",
    foreground: "#c0caf5",
    muted: "#9aa5ce",
    accent: "#7aa2f7",
    healthy: "#9ece6a",
    attention: "#e0af68",
    critical: "#f7768e",
};

/// Cream-background light theme. WCAG AA verified.
pub const LIGHT: Theme = Theme {
    name: ThemeName::Light,
    background: "#e6e2cf",
    background_raised: "#d8d2bb",
    foreground: "#2c2c2a",
    muted: "#5f5e5a",
    accent: "#185fa5",
    healthy: "#3b6d11",
    attention: "#854f0b",
    critical: "#a32d2d",
};

/// High-contrast theme. WCAG AAA.
pub const HIGH_CONTRAST: Theme = Theme {
    name: ThemeName::HighContrast,
    background: "#000000",
    background_raised: "#1a1a1a",
    foreground: "#ffffff",
    muted: "#cccccc",
    accent: "#00ffff",
    healthy: "#00ff00",
    attention: "#ffff00",
    critical: "#ff0000",
};

// ============================================================================
// §12 — Sizing
// ============================================================================

/// Terminal sizing breakpoints.
pub mod sizing {
    /// Minimum supported terminal width. Below this, refuse to render.
    pub const MIN_COLS: u16 = 80;
    /// Minimum supported terminal height.
    pub const MIN_ROWS: u16 = 24;

    /// Width above which the TUI uses the standard layout.
    pub const STANDARD_COLS: u16 = 120;
    /// Height above which the TUI uses the standard layout.
    pub const STANDARD_ROWS: u16 = 40;

    /// Width above which the TUI may use two-column workload layout.
    pub const WIDE_COLS: u16 = 160;

    /// Card overlays lock at this width regardless of terminal size.
    pub const CARD_WIDTH: u16 = 64;
    /// Minimum card height.
    pub const CARD_HEIGHT_MIN: u16 = 8;
    /// Maximum card height.
    pub const CARD_HEIGHT_MAX: u16 = 22;
}

// ============================================================================
// Compile-time invariants
// ============================================================================
// These fire on every `cargo build`, not just `cargo test`. Module-scope
// const-asserts replaced the earlier `const { assert!(...) }` form inside
// the test module after Windows A reported clippy 1.91's
// assertions_on_constants firing on the inline-const form (UX-CAR-001).

const _: () = assert!(thresholds::VRAM_CRITICAL_PCT >= thresholds::VRAM_ATTENTION_PCT);
const _: () = assert!(thresholds::RAM_CRITICAL_PCT >= thresholds::RAM_ATTENTION_PCT);
const _: () = assert!(thresholds::KV_CRITICAL_PCT >= thresholds::KV_ATTENTION_PCT);
const _: () = assert!(sizing::STANDARD_COLS >= sizing::MIN_COLS);
const _: () = assert!(sizing::WIDE_COLS >= sizing::STANDARD_COLS);
const _: () = assert!(sizing::CARD_HEIGHT_MAX >= sizing::CARD_HEIGHT_MIN);
const _: () = assert!(sizing::CARD_WIDTH < sizing::MIN_COLS);
const _: () = assert!(power::DEFAULT_KWH_RATE_USD > 0.0);
const _: () = assert!(limits::ACTIVITY_FEED_TUI_MAX <= limits::ACTIVITY_FEED_WIRE_MAX);
const _: () = assert!(limits::ACTIVITY_FEED_WEB_MAX <= limits::ACTIVITY_FEED_WIRE_MAX);
const _: () = assert!(limits::ACTIVITY_FEED_TUI_MAX > 0);
const _: () = assert!(limits::ACTIVITY_FEED_WIRE_MAX > 0);
const _: () = assert!(limits::ACTIVITY_FEED_WEB_MAX > 0);
const _: () = assert!(thresholds::THERMAL_AMBER_C > 0.0);
const _: () = assert!(thresholds::THERMAL_RED_C > thresholds::THERMAL_AMBER_C);
const _: () = assert!(limits::REC_MAX_VISIBLE <= ALERT_MAX_VISIBLE);
const _: () = assert!(limits::REC_MAX_VISIBLE > 0);
const _: () = assert!(limits::REC_TARGETS_MAX > 0);

// ============================================================================
// Tests — verify the contract is internally consistent
// ============================================================================
// `thresholds_are_ordered` and `sizing_is_consistent` were removed in 0.3.1;
// the module-scope const-asserts above enforce those invariants at every
// build, making the test-mod equivalents redundant.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_version_matches_doc() {
        // If you change CONTRACT_VERSION, update UX_CONTRACT.md to match.
        assert_eq!(CONTRACT_VERSION, "0.3.22");
    }

    #[test]
    fn all_alert_ids_have_a_template() {
        // Defensive: every AlertId variant should be reachable in alerts::* by
        // the dispatch layer. This test enumerates so a reviewer notices when
        // a new variant is added without a template.
        // CAR-24 (v0.3.15): ThermalPressure added; count 6 → 7.
        let all = [
            AlertId::VramPressure,
            AlertId::RamPressure,
            AlertId::KvPressure,
            AlertId::GovernorArmed,
            AlertId::OomDetected,
            AlertId::WorkloadExited,
            AlertId::ThermalPressure,
        ];
        assert_eq!(all.len(), 7, "AlertId count changed — update templates");
    }

    #[test]
    fn thermal_pressure_template_is_system_scope() {
        // CAR-24: thermal pressure is system-scope. Per Inspector
        // v1.2.0 design §1b option (a), the template MUST NOT
        // carry a per-PID attribution placeholder — host thermal
        // is whole-die / zone-level and naming a "responsible"
        // PID would be a causal-attribution guess that the
        // observe-only authority lock explicitly avoids.
        let t = alerts::THERMAL_PRESSURE;
        assert!(!t.is_empty());
        assert!(
            !t.contains("{pid}"),
            "system-scope template must not contain {{pid}}: {t:?}"
        );
        assert!(
            !t.contains("{workload}"),
            "system-scope template must not contain {{workload}}: {t:?}"
        );
        assert!(
            t.contains("{temp_c}"),
            "thermal template must carry {{temp_c}} placeholder: {t:?}"
        );
    }

    #[test]
    fn themes_have_distinct_palettes() {
        assert_ne!(DARK.background, LIGHT.background);
        assert_ne!(LIGHT.background, HIGH_CONTRAST.background);
    }

    #[test]
    fn workload_status_symbols_distinct() {
        let symbols = [
            WorkloadStatus::Healthy.symbol(),
            WorkloadStatus::Attention.symbol(),
            WorkloadStatus::Critical.symbol(),
            WorkloadStatus::Loading.symbol(),
        ];
        let mut sorted: Vec<&'static str> = symbols.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 4);
    }

    #[test]
    fn help_keys_match_action_variants() {
        // Every Action variant should have a corresponding help::KEY_*
        // constant. If someone adds a new Action without documenting it
        // in the help overlay, the length mismatch fires.
        let actions = [
            Action::Quit,
            Action::ToggleHelp,
            Action::SelectUp,
            Action::SelectDown,
            Action::KillOrConfirm,
            Action::OpenDetail,
            Action::ToggleHistory,
            Action::AcknowledgeAlerts,
            Action::CycleTopSort,
            Action::EscapeCascade,
            Action::ToggleActivityBrowse,
            Action::ToggleHistoryEvents,
        ];
        let help_keys = [
            help::KEY_QUIT,
            help::KEY_HELP,
            help::KEY_SELECT_UP,
            help::KEY_SELECT_DOWN,
            help::KEY_KILL,
            help::KEY_DETAIL,
            help::KEY_HISTORY,
            help::KEY_ACK_ALERTS,
            help::KEY_TOP_SORT,
            help::KEY_ESC,
            help::KEY_ACTIVITY_BROWSE,
            help::KEY_HISTORY_EVENTS,
        ];
        assert_eq!(
            actions.len(),
            help_keys.len(),
            "Action variant count drifted from help::KEY_* count"
        );
        for k in help_keys {
            assert!(!k.is_empty(), "help::KEY_* contains an empty string");
        }
    }

    /// CAR-D97 / DISPATCH 97 — the TUI history-events overlay copy
    /// strings must carry their `{time}` / `{count}` substitution
    /// tokens. Consumers rely on these — a rename that drops the
    /// token would silently print the literal `{time}` in the header.
    /// The mnemonic "H" also appears in the TITLE so the operator
    /// sees which key opened this surface.
    #[test]
    fn history_events_copy_strings_carry_substitution_tokens() {
        assert!(history_events::TITLE.contains("{time}"));
        assert!(history_events::TITLE.contains("H"));
        assert!(!history_events::EMPTY.is_empty());
        assert!(history_events::RELOAD_TEMPLATE.contains("{count}"));
    }

    /// CAR-D97 — lowercase `h` (Action::ToggleHistory, RunStore
    /// per-model overlay) and capital `H` (Action::ToggleHistoryEvents,
    /// event archive browser) are SEPARATE surfaces on the contract.
    /// The KEY_* constants must both exist and be non-empty; the
    /// enum variants must be distinct. If a future edit collapses
    /// one into the other, this pin fires.
    #[test]
    fn history_and_history_events_are_distinct_surfaces() {
        assert_ne!(Action::ToggleHistory, Action::ToggleHistoryEvents);
        assert!(!help::KEY_HISTORY.is_empty());
        assert!(!help::KEY_HISTORY_EVENTS.is_empty());
        // The two help strings shouldn't be identical — they name
        // different surfaces and the operator reads both in the ?
        // overlay.
        assert_ne!(help::KEY_HISTORY, help::KEY_HISTORY_EVENTS);
    }

    #[test]
    fn postmortem_labels_are_nonempty() {
        use postmortem_labels::*;
        for s in [
            CAUSE, RUNTIME, THROUGHPUT, PEAK_RAM, PEAK_VRAM, KV_CACHE, ENERGY, LAST_STDERR,
            FOOTER, KILL_ACTION, KILL_RESULT,
        ] {
            assert!(!s.is_empty(), "postmortem_labels constant is empty");
        }
    }

    #[test]
    fn vram_unmeasured_is_not_a_zero_value() {
        // Honesty lock (CAR-D75): the unmeasured-VRAM placeholder MUST NOT
        // be a literal zero string. The renderer is forbidden from
        // substituting "0 MB" for an unmeasured field — a zero reads as
        // "measured and empty", which overclaims.
        let s = status::VRAM_UNMEASURED.to_lowercase();
        for zero in ["0 mb", "0mb", "0 gb", "0gb", "0%", "0 b"] {
            assert!(
                !s.contains(zero),
                "VRAM_UNMEASURED must not contain a zero-value string {zero:?}: {:?}",
                status::VRAM_UNMEASURED
            );
        }
        assert!(!status::VRAM_UNMEASURED.is_empty());
    }

    #[test]
    fn postmortem_kill_labels_are_nonempty() {
        // CAR-D75: the kill-action/result labels follow the same
        // nonempty discipline as the rest of postmortem_labels.
        use postmortem_labels::*;
        for s in [KILL_ACTION, KILL_RESULT] {
            assert!(!s.is_empty(), "postmortem_labels kill constant is empty");
        }
    }

    #[test]
    fn workload_category_headers_well_formed() {
        // Defensive: every group-header constant follows the canonical
        // "── X ──" shape. Catches typos (missing marker, wrong dash
        // character, swapped order) at test time.
        use workload_category::*;
        for h in [
            GROUP_HEADER_LLM,
            GROUP_HEADER_VISION,
            GROUP_HEADER_ROS2,
            GROUP_HEADER_EMBEDDINGS,
            GROUP_HEADER_AGENT,
            GROUP_HEADER_UNKNOWN,
        ] {
            assert!(h.starts_with("── "), "header missing leading marker: {:?}", h);
            assert!(h.ends_with(" ──"), "header missing trailing marker: {:?}", h);
            assert!(
                h.len() > "── ── ".len(),
                "header has no category name between markers: {:?}",
                h
            );
        }
    }

    #[test]
    fn agent_header_present_and_well_formed() {
        // CAR-18: GROUP_HEADER_AGENT exists, is non-empty, follows the
        // canonical "── X ──" shape, and the category name between
        // the markers is literally "Agent". This is the constants-
        // pattern equivalent of "Agent variant serializes to string"
        // / "Display impl yields Agent" — the contract uses string
        // constants per CAR-8's documented design, not a Display impl
        // on a contract-owned enum.
        let h = workload_category::GROUP_HEADER_AGENT;
        assert!(!h.is_empty(), "GROUP_HEADER_AGENT is empty");
        assert!(h.starts_with("── "), "missing leading marker: {h:?}");
        assert!(h.ends_with(" ──"), "missing trailing marker: {h:?}");
        assert!(
            h.contains("Agent"),
            "header must contain literal 'Agent': {h:?}"
        );
    }

    #[test]
    fn agent_header_distinct_from_llm() {
        // CAR-18 regression guard: the entire point of CAR-18 is that
        // Agent is *not* LLM. If a future edit ever makes these strings
        // equal (e.g. someone reverts the split by aliasing AGENT to
        // LLM), this fires.
        assert_ne!(
            workload_category::GROUP_HEADER_AGENT,
            workload_category::GROUP_HEADER_LLM,
            "Agent header must not collide with LLM header"
        );
    }

    #[test]
    fn agent_header_distinct_from_all_other_categories() {
        // CAR-18: every category header must be globally distinct —
        // otherwise the workloads panel would render two sections with
        // the same title and users couldn't tell them apart. Covers
        // the "all variants have a unique display" intent.
        use workload_category::*;
        let headers = [
            GROUP_HEADER_LLM,
            GROUP_HEADER_VISION,
            GROUP_HEADER_ROS2,
            GROUP_HEADER_EMBEDDINGS,
            GROUP_HEADER_AGENT,
            GROUP_HEADER_UNKNOWN,
        ];
        let mut sorted: Vec<&'static str> = headers.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            headers.len(),
            "workload_category headers must all be distinct"
        );
    }

    #[test]
    fn all_workload_category_headers_count_includes_agent() {
        // CAR-18: enumerating from the module ensures Agent is in the
        // canonical category set. If someone adds a future category
        // (e.g. Audio) without updating this count, the count assertion
        // fires and forces a review — same pattern as
        // `all_alert_ids_have_a_template`. v0.3.9 ships 6 categories.
        use workload_category::*;
        let headers = [
            GROUP_HEADER_LLM,
            GROUP_HEADER_VISION,
            GROUP_HEADER_ROS2,
            GROUP_HEADER_EMBEDDINGS,
            GROUP_HEADER_AGENT,
            GROUP_HEADER_UNKNOWN,
        ];
        assert_eq!(
            headers.len(),
            6,
            "workload_category header count changed — update consumers and bump CONTRACT_VERSION"
        );
        assert!(
            headers.contains(&GROUP_HEADER_AGENT),
            "Agent must be in the canonical workload_category header set"
        );
    }

    #[test]
    fn degraded_line_templates_well_formed() {
        // CAR-9: every category that has a baseline-comparable schema
        // (LLM/Vision/Embeddings) must include `baseline` and a signed
        // `{delta_pct}` placeholder per §2. Catches accidental edits
        // that strip one or the other.
        use degraded_line::*;
        for s in [LLM, VISION, EMBEDDINGS] {
            assert!(
                s.contains("baseline"),
                "template missing baseline: {s:?}"
            );
            assert!(
                s.contains("{delta_pct}"),
                "template missing {{delta_pct}} placeholder: {s:?}"
            );
            assert!(
                s.contains(" · "),
                "template missing `·` separator: {s:?}"
            );
        }
        // Unknown: short fixed message, no placeholders.
        assert!(!UNKNOWN.is_empty());
        assert!(
            !UNKNOWN.contains('{'),
            "UNKNOWN must not contain a placeholder"
        );
        // ROS2: empty for v1.0 per §2 (deferred to v1.1+). If a future
        // edit accidentally populates this without bumping the
        // contract, this assert flags it.
        assert!(
            ROS2.is_empty(),
            "ROS2 template is locked empty for v1.0 — §2 defers to v1.1"
        );
    }

    #[test]
    fn top_processes_labels_distinct_and_nonempty() {
        // CAR-11: the panel title is composed as
        // `"{PANEL_TITLE_PREFIX} (by {SORT_BY_*})"`. The three sort
        // labels must be distinct so the title and `TOP_SORT_CHANGED`
        // footer message don't render an ambiguous cycle state.
        use top_processes::*;
        assert!(!PANEL_TITLE_PREFIX.is_empty());
        let labels = [SORT_BY_RAM, SORT_BY_CPU, SORT_BY_VRAM];
        for l in labels {
            assert!(!l.is_empty(), "SORT_BY_* label is empty");
        }
        let mut sorted: Vec<&'static str> = labels.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "SORT_BY_* labels must be distinct");
    }

    #[test]
    fn mission_template_carries_n_and_m_placeholders() {
        // CAR-13: the §0 mission line composes `{n}` (total workloads)
        // and `{m}` (degraded count) into a single header string.
        // Renderer substitutes both at render time; if a future edit
        // drops either placeholder the header will silently stop
        // showing one of the counts.
        let t = mission::TEMPLATE;
        assert!(t.contains("{n}"), "mission::TEMPLATE missing {{n}}: {t:?}");
        assert!(t.contains("{m}"), "mission::TEMPLATE missing {{m}}: {t:?}");
    }

    #[test]
    fn mission_template_is_nonempty() {
        assert!(!mission::TEMPLATE.is_empty());
    }

    #[test]
    fn mission_template_matches_l25_w46_shipped_string() {
        // CAR-13 lock-in: the literal mission-line shipped in Linux L25
        // (`f15a5f7`) and Windows W46 (`eb9f5a0`) is reproduced
        // verbatim. If anyone edits mission::TEMPLATE without a
        // coordinated update to both consumers, this assertion fires
        // and prevents the contract from drifting silently away from
        // the strings that L25 / W46 already render.
        assert_eq!(
            mission::TEMPLATE,
            "raqib · {n} workloads · {m} degraded · press ? for help"
        );
    }

    #[test]
    fn kill_keybinding_arm_key_is_k() {
        // CAR-14: arm key is locked to lowercase 'k'. The keymap
        // section of §6 binds 'k' (lowercase) to KillOrConfirm in the
        // workloads pane; uppercase 'K' is the SelectUp alias.
        assert_eq!(kill_keybinding::ARM_KEY, 'k');
    }

    #[test]
    fn kill_keybinding_confirm_is_enter() {
        // CAR-14: confirm key is Enter, not a repeated 'k'. Double-k
        // was unreliable in user testing; this lock-in prevents a
        // revert to the prior pattern without a follow-up CAR.
        assert_eq!(kill_keybinding::CONFIRM_KEY_NAME, "Enter");
    }

    #[test]
    fn kill_keybinding_banner_mentions_enter_and_esc() {
        // CAR-14: the armed-state banner must mention both the confirm
        // key and the cancel key — otherwise users don't know how to
        // exit the armed state. Catches accidental edits that drop one.
        let b = kill_keybinding::ARMED_BANNER;
        assert!(
            b.contains(kill_keybinding::CONFIRM_KEY_NAME),
            "ARMED_BANNER missing confirm key {:?}: {b:?}",
            kill_keybinding::CONFIRM_KEY_NAME
        );
        assert!(
            b.contains(kill_keybinding::CANCEL_KEY_NAME),
            "ARMED_BANNER missing cancel key {:?}: {b:?}",
            kill_keybinding::CANCEL_KEY_NAME
        );
    }

    #[test]
    fn kill_keybinding_arm_is_distinct_from_confirm() {
        // CAR-14: arm and confirm must be different keys (that's the
        // whole point of moving off double-k). If a future edit makes
        // CONFIRM_KEY_NAME equal "k" or "K", this fires.
        let arm_str = kill_keybinding::ARM_KEY.to_string();
        assert_ne!(kill_keybinding::CONFIRM_KEY_NAME, arm_str.as_str());
        assert!(
            !kill_keybinding::CONFIRM_KEY_NAME
                .eq_ignore_ascii_case(arm_str.as_str()),
            "CONFIRM_KEY_NAME must not be a case-variant of ARM_KEY"
        );
    }

    #[test]
    fn kill_confirm_constants_nonempty() {
        // CAR-17: every kill_confirm surface string is user-visible —
        // empty strings would render a broken card. Catches accidental
        // edits that strip a constant's text.
        use kill_confirm_card::*;
        for s in [
            KILL_CONFIRM_TITLE,
            KILL_CONFIRM_PROMPT,
            KILL_CONFIRM_HINT,
            KILL_CONFIRM_PID_LABEL,
            KILL_CONFIRM_WORKLOAD_LABEL,
            KILL_CONFIRM_CATEGORY_LABEL,
            KILL_CONFIRM_STATUS_LABEL,
            KILL_CONFIRM_RUNTIME_LABEL,
            KILL_CONFIRM_RAM_LABEL,
            KILL_CONFIRM_VRAM_LABEL,
            KILL_CONFIRM_CPU_LABEL,
            // CAR-D82 (v0.3.19) escalation-state strings.
            KILL_CONFIRM_WAITING_PROMPT,
            KILL_CONFIRM_WAITING_HINT,
            KILL_CONFIRM_FORCE_SIGKILL,
        ] {
            assert!(!s.is_empty(), "kill_confirm_card constant is empty: {s:?}");
        }
    }

    #[test]
    fn kill_confirm_prompt_uses_question_mark() {
        // CAR-17: the prompt must read as a question so the user
        // explicitly answers yes/no rather than acknowledging a
        // statement. If a future edit strips the '?' (or rewrites the
        // prompt as a declarative), the answer-shaped UX breaks.
        let p = kill_confirm_card::KILL_CONFIRM_PROMPT;
        assert!(p.contains('?'), "KILL_CONFIRM_PROMPT missing '?': {p:?}");
    }

    #[test]
    fn kill_confirm_hint_lists_both_enter_and_esc() {
        // CAR-17: the card's footer hint must document both exits —
        // confirm (Enter) and cancel (Esc) — so users always see how to
        // leave the card. Catches edits that drop one key.
        let h = kill_confirm_card::KILL_CONFIRM_HINT;
        assert!(h.contains("Enter"), "KILL_CONFIRM_HINT missing Enter: {h:?}");
        assert!(h.contains("Esc"), "KILL_CONFIRM_HINT missing Esc: {h:?}");
    }

    #[test]
    fn kill_confirm_no_dry_run_reference() {
        // CAR-17: dry-run mode is being removed entirely — kill is
        // always real, the card IS the safety. No kill_confirm string
        // may reference "dry" in any form. If a future edit reintroduces
        // a dry-run-flavored variant (case-insensitive), this fires.
        use kill_confirm_card::*;
        for s in [
            KILL_CONFIRM_TITLE,
            KILL_CONFIRM_PROMPT,
            KILL_CONFIRM_HINT,
            KILL_CONFIRM_PID_LABEL,
            KILL_CONFIRM_WORKLOAD_LABEL,
            KILL_CONFIRM_CATEGORY_LABEL,
            KILL_CONFIRM_STATUS_LABEL,
            KILL_CONFIRM_RUNTIME_LABEL,
            KILL_CONFIRM_RAM_LABEL,
            KILL_CONFIRM_VRAM_LABEL,
            KILL_CONFIRM_CPU_LABEL,
            // CAR-D82 (v0.3.19) escalation-state strings.
            KILL_CONFIRM_WAITING_PROMPT,
            KILL_CONFIRM_WAITING_HINT,
            KILL_CONFIRM_FORCE_SIGKILL,
        ] {
            assert!(
                !s.to_lowercase().contains("dry"),
                "kill_confirm_card constant references dry-run: {s:?}"
            );
        }
    }

    #[test]
    fn kill_confirm_waiting_prompt_interpolates_grace_seconds() {
        // CAR-D82: the Waiting-state prompt must carry the `{secs}`
        // render-time placeholder (NOT a split const + format!) so the
        // grace countdown matches the contract's interpolation convention
        // (status::KILL_ARMED / status::KILL_COUNTDOWN). If a future edit
        // hard-codes a number or drops the placeholder, this fires.
        let p = kill_confirm_card::KILL_CONFIRM_WAITING_PROMPT;
        assert!(
            p.contains("{secs}"),
            "WAITING_PROMPT must interpolate {{secs}}: {p:?}"
        );
        // Honesty lock: the prompt must tell the operator the graceful
        // signal already went out — otherwise the wait reads as "nothing
        // happened". The catchable first signal is SIGTERM.
        assert!(
            p.contains("SIGTERM"),
            "WAITING_PROMPT must name the SIGTERM already sent: {p:?}"
        );
    }

    #[test]
    fn kill_confirm_waiting_hint_lists_force_and_cancel() {
        // CAR-D82: the Waiting-state footer must document both grace-window
        // exits — Enter escalates (force), Esc cancels — mirroring the
        // kill_confirm_hint_lists_both_enter_and_esc discipline.
        let h = kill_confirm_card::KILL_CONFIRM_WAITING_HINT;
        assert!(h.contains("Enter"), "WAITING_HINT missing Enter: {h:?}");
        assert!(h.contains("Esc"), "WAITING_HINT missing Esc: {h:?}");
        assert!(
            h.to_lowercase().contains("force"),
            "WAITING_HINT must name the force escalation: {h:?}"
        );
    }

    #[test]
    fn kill_confirm_force_label_names_sigkill() {
        // CAR-D82: informed-consent lock. The force action label must name
        // the signal (SIGKILL) explicitly so the operator knows this is the
        // uncatchable kill, distinct from the catchable SIGTERM sent first.
        let l = kill_confirm_card::KILL_CONFIRM_FORCE_SIGKILL;
        assert!(
            l.contains("SIGKILL"),
            "FORCE_SIGKILL label must name SIGKILL: {l:?}"
        );
    }

    #[test]
    fn kill_force_sent_is_distinct_sigkill_footer() {
        // CAR-D82: the escalated-path footer (status::KILL_FORCE_SENT) must
        // be a real SIGKILL counterpart to the graceful status::KILL_SENT —
        // naming SIGKILL, carrying the {name}/{pid} placeholders, and NOT
        // collapsing into the SIGTERM message (which would make the footer
        // lie about which signal fired).
        let s = status::KILL_FORCE_SENT;
        assert!(s.contains("SIGKILL"), "KILL_FORCE_SENT must name SIGKILL: {s:?}");
        assert!(s.contains("{name}"), "KILL_FORCE_SENT must carry {{name}}: {s:?}");
        assert!(s.contains("{pid}"), "KILL_FORCE_SENT must carry {{pid}}: {s:?}");
        assert_ne!(
            s, status::KILL_SENT,
            "KILL_FORCE_SENT must be distinct from the SIGTERM KILL_SENT"
        );
        assert!(
            !s.contains("SIGTERM"),
            "the SIGKILL footer must not name SIGTERM: {s:?}"
        );
    }

    #[test]
    fn history_column_header_is_fixed_width() {
        // Renderer assumes the header has no tabs / control whitespace —
        // alignment is by space-padding only.
        let h = history_labels::COLUMN_HEADER;
        assert!(!h.contains('\t'), "no tabs allowed in column header");
        assert!(!h.contains('\n'));
        assert!(!h.contains('\r'));
        for ch in h.chars() {
            if ch.is_whitespace() {
                assert_eq!(
                    ch, ' ',
                    "non-space whitespace {:?} found in COLUMN_HEADER",
                    ch
                );
            }
        }
    }

    // ----------------------------------------------------------------
    // CAR-19a — RUNNING_ACTIVELY constant tests (B-NEW-5)
    // ----------------------------------------------------------------

    #[test]
    fn running_actively_const_is_present_and_non_empty() {
        // CAR-19a: the literal `"running actively"` was duplicated
        // between the TUI workloads panel and the web WorkloadRow
        // component. Lifting it here as `status::RUNNING_ACTIVELY`
        // gives both consumers a single source of truth.
        assert!(!status::RUNNING_ACTIVELY.is_empty());
        assert_eq!(status::RUNNING_ACTIVELY, "running actively");
    }

    #[test]
    fn running_actively_const_distinct_from_other_status_strings() {
        // CAR-19a: RUNNING_ACTIVELY must not collide with the other
        // row-position placeholders, otherwise the renderer couldn't
        // distinguish them in tests/screenshots. COLD_LOADING is the
        // closest neighbor (same row position, different state).
        // CAR-20 (v0.3.11) added AGENT_ALIVE — same row position,
        // Agent-specific override — so it joins the distinctness set.
        assert_ne!(status::RUNNING_ACTIVELY, status::COLD_LOADING);
        assert_ne!(status::RUNNING_ACTIVELY, status::AGENT_ALIVE);
        for s in [
            status::KILL_ARMED,
            status::KILL_SENT,
            status::KILL_DISARMED,
            status::GOVERNOR_BLOCKED,
            status::ALERTS_ACKNOWLEDGED,
            status::NO_DETAIL_FOR_SYSTEM,
            status::TOP_SORT_CHANGED,
            status::FOOTER_KEYMAP,
            status::KILL_COUNTDOWN,
            status::KILL_ALLOWLIST_PREFIX,
            status::NO_WORKLOAD_FOCUSED,
        ] {
            assert_ne!(
                status::RUNNING_ACTIVELY, s,
                "RUNNING_ACTIVELY collides with another status string"
            );
        }
    }

    // ----------------------------------------------------------------
    // CAR-20 — AGENT_ALIVE constant tests (v0.3.11)
    // ----------------------------------------------------------------

    #[test]
    fn agent_alive_const_is_present_and_non_empty() {
        // CAR-20: the literal `"alive"` was duplicated between the
        // TUI workloads panel (workloads.rs::AGENT_ALIVE) and the
        // web WorkloadRow component. Lifting it here as
        // `status::AGENT_ALIVE` gives both consumers a single
        // source of truth.
        assert!(!status::AGENT_ALIVE.is_empty());
        assert_eq!(status::AGENT_ALIVE, "alive");
    }

    #[test]
    fn agent_alive_const_distinct_from_running_actively() {
        // CAR-20 regression guard: the entire point of AGENT_ALIVE
        // is that Agent rows display the honest-minimum "alive"
        // instead of the activity-claiming "running actively". If a
        // future edit ever aliases these (e.g. someone reverts the
        // split by pointing AGENT_ALIVE at RUNNING_ACTIVELY), the
        // overclaiming behavior comes back and this test fires.
        assert_ne!(
            status::AGENT_ALIVE,
            status::RUNNING_ACTIVELY,
            "AGENT_ALIVE must remain distinct from RUNNING_ACTIVELY"
        );
    }

    // ----------------------------------------------------------------
    // CAR-19b — dry-run + Grafana removal markers (B-NEW-7 + carried)
    // ----------------------------------------------------------------
    //
    // These tests are discoverability markers: the compiler is what
    // actually enforces absence (any re-add would compile fine — these
    // tests are not affected). The markers exist so a future revert
    // (deleting the test or re-introducing the constants) surfaces in
    // review next to the surface itself.

    #[test]
    fn dry_run_constants_no_longer_exist() {
        // CAR-19b (v0.3.10): KILL_DRY_RUN and KILL_DRY_RUN_PREFIX were
        // removed. Sprint 1's dry-run feature is gone — kill is now
        // never-faked. KILL_ALLOWLIST_PREFIX remains (allowlist
        // override is a separate feature). Touching the surviving
        // surface here pins the post-removal shape: if anyone re-adds
        // dry-run footer messages, they'll need to look at this test
        // in review.
        assert_eq!(status::KILL_ALLOWLIST_PREFIX, "Allowlist override — ");
    }

    #[test]
    fn grafana_constants_no_longer_exist() {
        // CAR-19b (v0.3.10): DASHBOARD_OPENED, DASHBOARD_FAILED,
        // GRAFANA_UNREACHABLE constants and Action::OpenGrafana /
        // help::KEY_GRAFANA were removed. Sprint 5 hard-deleted the
        // Grafana integration; the constants lingered until v0.3.10.
        // FOOTER_KEYMAP's "g graph" token was also dropped — this
        // assertion locks the post-removal shape.
        assert!(
            !status::FOOTER_KEYMAP.contains("graph"),
            "FOOTER_KEYMAP must not advertise the removed `g graph` binding: {:?}",
            status::FOOTER_KEYMAP
        );
        assert!(
            !status::FOOTER_KEYMAP.to_lowercase().contains("grafana"),
            "FOOTER_KEYMAP must not mention grafana: {:?}",
            status::FOOTER_KEYMAP
        );
    }

    // ----------------------------------------------------------------
    // CAR-19c — Activity-feed caps (B-NEW-10)
    // ----------------------------------------------------------------

    // Note: the three activity_feed_* tests below compare const-vs-const
    // values and trigger clippy::assertions_on_constants — the same
    // lint UX-CAR-001 documented. The module-scope `const _: () =
    // assert!(...)` block above is what actually enforces these
    // invariants at compile time (and runs on every cargo build, not
    // just cargo test). The runtime tests are kept here because the
    // orchestrator's CAR-19c brief enumerated them by name, and they
    // serve as a discoverability surface for the invariants.
    // Per-test `#[allow]` is the targeted suppression rather than
    // disabling the lint workspace-wide.

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn activity_feed_caps_are_positive() {
        // CAR-19c: a zero-cap would silently truncate the feed to
        // nothing. Compile-time const-assert at module scope also
        // enforces this — runtime test is a discoverability marker.
        assert!(limits::ACTIVITY_FEED_TUI_MAX > 0);
        assert!(limits::ACTIVITY_FEED_WIRE_MAX > 0);
        assert!(limits::ACTIVITY_FEED_WEB_MAX > 0);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn activity_feed_tui_max_less_than_or_equal_wire_max() {
        // CAR-19c: WIRE bounds what's shipped between processes;
        // TUI/WEB caps must not exceed it. See module doc-comment.
        assert!(
            limits::ACTIVITY_FEED_TUI_MAX <= limits::ACTIVITY_FEED_WIRE_MAX,
            "TUI cap {} must not exceed WIRE cap {}",
            limits::ACTIVITY_FEED_TUI_MAX,
            limits::ACTIVITY_FEED_WIRE_MAX
        );
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn activity_feed_web_max_less_than_or_equal_wire_max() {
        // CAR-19c: see activity_feed_tui_max_less_than_or_equal_wire_max.
        assert!(
            limits::ACTIVITY_FEED_WEB_MAX <= limits::ACTIVITY_FEED_WIRE_MAX,
            "WEB cap {} must not exceed WIRE cap {}",
            limits::ACTIVITY_FEED_WEB_MAX,
            limits::ACTIVITY_FEED_WIRE_MAX
        );
    }
}
