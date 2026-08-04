//! v0.3.20 CAR-D93 — History wire surface (PHASE 5 step 6 gate).
//!
//! Anchor for the `/api/history` endpoint the consumer builds in a
//! follow-up dispatch. This module holds the CONTRACT SIDE of
//! History: path constants, canonical wire field names, capacity
//! limits, and the documented shape decisions (Q1–Q4 in the CAR).
//!
//! Per the crate's zero-dependency stance, the actual wire STRUCTS
//! (with `serde` derives) live in the consumer (edge_monitor's
//! `src/web/wire.rs`), not here. The contract holds the shape's
//! discriminators + constants + reference wire-string mappings —
//! same split as `activity` (v0.3.17) and `host_vitals` (v0.3.13).
//!
//! ## Two-endpoint split (Q2)
//!
//! History reads split across TWO endpoints — deliberately not a
//! single fat response:
//!
//! * [`PATH`] `/api/history` — the SNAPSHOT: cross-PID event
//!   timeline (`Vec<WireHistoryEvent>`, cap
//!   [`EVENT_ARCHIVE_MAX`]) + a lightweight dead-PID index
//!   (recently-exited AI PIDs the operator can drill into). Small
//!   response (~150 KB worst case at cap × ~300 B/entry). Safe to
//!   fetch on view open.
//! * [`PATH_TRAJECTORY_PREFIX`] `/api/history/trajectory/{pid}` —
//!   PER-DEAD-PID full trajectory (up to
//!   [`TRAJECTORY_SAMPLES_PER_PID_MAX`] samples). Fetched on
//!   demand when the operator selects a dead PID from the index.
//!   28 B × 1800 = ~50 KB per response. Bounded, lazy.
//!
//! Rejected alternative: single fat `/api/history` embedding all
//! recent trajectories. 50 dead × 1800 samples × 28 B = 2.5 MB per
//! request; a snapshot-on-open view fetches it every time the
//! operator hits Reload. The lazy-load shape is strictly cheaper
//! without giving up any information.
//!
//! ## Event kinds — REUSE [`crate::activity::ActivityKind`] (Q1)
//!
//! History events carry the SAME three-source discriminator the
//! activity feed uses (`Exit` / `Kill` / `Regression` — v0.3.17
//! CAR-71). NO new History-specific kind enum. If a future fourth
//! event source ever lands (alert raise/ack? governor rate-limit
//! defer?), it must bump `ActivityKind` and both surfaces adopt
//! together — this keeps the operator's mental model unified across
//! the live feed and the history view.
//!
//! Reference-string test [`tests::reference_activity_kind_wire_strings_still_hold`]
//! pins the History surface against a silent
//! [`crate::activity::ActivityKind`] rename.
//!
//! ## Event severity — REUSE [`crate::activity::ActivitySeverity`]
//!
//! Same rationale as kind: the live feed and history view use the
//! same three-band Healthy/Attention/Critical classifier so the
//! operator sees consistent color across surfaces.
//!
//! ## VRAM honesty on the wire (Q3)
//!
//! The consumer's `Sample` type carries `vram_mb: Option<u32>` —
//! `None` for unmeasured (the operator's driver-unloaded case,
//! `NVML_UNMEASURED` per D74/D78). The wire MUST preserve that
//! discriminator: serialize `None` as JSON `null` or omit the
//! field entirely (`#[serde(skip_serializing_if = "Option::is_none")]`).
//! It MUST NOT default-fill to `0` — a `0` reading would be
//! indistinguishable from a real "the workload used 0 MB of VRAM
//! this tick," which is the exact confusion the [`VRAM_UNMEASURED`]
//! display marker exists to prevent.
//!
//! The consumer's serde default for `Option<T>` on serialize is
//! `null`, which is correct. The pin here is the DOCUMENTED
//! requirement + the reference test
//! [`tests::vram_unmeasured_convention_notes_null_not_zero`].
//!
//! ## Canonical wire field names
//!
//! Consumer serde derives use these strings as JSON keys. Centralised
//! here so a rename shows up as a single CAR bump rather than a
//! silent per-consumer drift (the D71 activity-feed lesson).

// ─────────────────────────────────────────────────────────────────
// Endpoint paths
// ─────────────────────────────────────────────────────────────────

/// GET path for the History snapshot: events + dead-PID index.
/// D85-auth-gated on the consumer side (as with every `/api/*`
/// route since D85).
pub const PATH: &str = "/api/history";

/// Prefix for the per-PID trajectory GET: consumer appends the
/// PID (`format!("{}{}", PATH_TRAJECTORY_PREFIX, pid)`). D85-auth-gated.
///
/// The prefix (not a full path with `{pid}` placeholder) lets the
/// consumer route with axum's `:pid` capture syntax without the
/// contract carrying a placeholder token that could drift from the
/// consumer's URL parser.
pub const PATH_TRAJECTORY_PREFIX: &str = "/api/history/trajectory/";

// ─────────────────────────────────────────────────────────────────
// Capacity limits
// ─────────────────────────────────────────────────────────────────

/// Recommended default cap on the per-PID trajectory sample ring
/// (the D89 consumer field
/// `runtime.history_trajectory_samples_per_pid` defaults to this).
/// 1800 samples ≈ 30 min at the 1 Hz tick rate — the empirical
/// "how far back is interesting on a live session" window the
/// PHASE 5 design doc identifies (Q2).
///
/// The consumer's config accepts values in `1..=18000` (10× the
/// default upper guard); this constant is the recommended default,
/// not a hard cap the consumer's validate() enforces.
pub const TRAJECTORY_SAMPLES_PER_PID_MAX: usize = 1800;

/// Recommended default cap on the cross-PID event archive (the D89
/// consumer field `runtime.history_event_archive_cap` defaults to
/// this). 500 entries ≈ ~150 KB per event × ~300 B — the on-demand
/// timeline read stays bounded without needing streaming.
///
/// Compare with [`crate::limits::ACTIVITY_FEED_WIRE_MAX`] = 50, the
/// LIVE snapshot cap on `/api/snapshot`. The 10× ratio between live
/// and archive is deliberate: the live wire ships every second, so
/// it stays skinny; the archive is fetched on operator action, so
/// it can carry deeper history without inflating routine traffic.
pub const EVENT_ARCHIVE_MAX: usize = 500;

// ─────────────────────────────────────────────────────────────────
// Canonical wire field names — consumer serde uses these as keys
// ─────────────────────────────────────────────────────────────────

/// Snapshot envelope: the event timeline field.
pub const KEY_EVENTS: &str = "events";
/// Snapshot envelope: the recently-exited AI PID index.
pub const KEY_DEAD_PIDS: &str = "dead_pids";

/// Event: kind discriminator, serialized as one of `"exit"` /
/// `"kill"` / `"regression"` — same strings the
/// [`crate::activity::ActivityKind`] reference test pins.
pub const KEY_EVENT_KIND: &str = "kind";
/// Event: RFC 3339 timestamp string.
pub const KEY_EVENT_TIMESTAMP: &str = "timestamp";
/// Event: PID (`u32`; `0` sentinel for model-scoped regression events
/// per the D71 activity-feed convention).
pub const KEY_EVENT_PID: &str = "pid";
/// Event: display name (workload comm for exit/kill; model name for
/// regression).
pub const KEY_EVENT_NAME: &str = "name";
/// Event: pre-rendered one-line summary (single source of truth —
/// consumer does NOT re-render on read).
pub const KEY_EVENT_SUMMARY: &str = "summary";
/// Event: severity discriminator, serialized as one of `"healthy"`
/// / `"attention"` / `"critical"` per
/// [`crate::activity::ActivitySeverity`].
pub const KEY_EVENT_SEVERITY: &str = "severity";

/// Trajectory: sample-array field.
pub const KEY_TRAJECTORY_SAMPLES: &str = "samples";
/// Trajectory: first-sample timestamp (RFC 3339 string).
pub const KEY_TRAJECTORY_FIRST_SAMPLE_AT: &str = "first_sample_at";
/// Trajectory: last-sample timestamp (RFC 3339 string).
pub const KEY_TRAJECTORY_LAST_SAMPLE_AT: &str = "last_sample_at";

/// Sample: RFC 3339 timestamp.
pub const KEY_SAMPLE_TIMESTAMP: &str = "timestamp";
/// Sample: CPU percent (`f32`).
pub const KEY_SAMPLE_CPU_PCT: &str = "cpu_pct";
/// Sample: RSS MB (`u32`). **Absent (or `null`) when unmeasured** —
/// see the VRAM honesty note.
pub const KEY_SAMPLE_RSS_MB: &str = "rss_mb";
/// Sample: VRAM MB (`Option<u32>`). **Serialize `None` as JSON
/// `null` or omit** — never as `0`. See the VRAM honesty note in
/// the module doc.
pub const KEY_SAMPLE_VRAM_MB: &str = "vram_mb";

/// Dead-PID index entry: the exited AI PID (drill-in key for
/// [`PATH_TRAJECTORY_PREFIX`]).
pub const KEY_DEAD_PID: &str = "pid";
/// Dead-PID index entry: workload display name.
pub const KEY_DEAD_PID_NAME: &str = "name";
/// Dead-PID index entry: model name (`Option<String>`); may be
/// omitted / `null` for workloads without a resolved model.
pub const KEY_DEAD_PID_MODEL: &str = "model_name";
/// Dead-PID index entry: exit timestamp (RFC 3339 string) — the
/// UI sort key.
pub const KEY_DEAD_PID_EXIT_TIME: &str = "exit_time";

// ─────────────────────────────────────────────────────────────────
// VRAM honesty marker — consumer's display when a sample reads
// `vram_mb = null`. Mirrors the existing `status::VRAM_UNMEASURED`
// (v0.3.18) but scoped to the history view so a reader can find it
// alongside the wire keys.
// ─────────────────────────────────────────────────────────────────

/// Display placeholder for `Sample::vram_mb == None` in the history
/// trajectory renderer. NOT a numeric value — the consumer's
/// renderer switches on the `Option::is_none` branch and prints
/// this string.
pub const VRAM_UNMEASURED: &str = "—";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{ActivityKind, ActivitySeverity};

    /// The two endpoint paths are stable strings, distinct, and
    /// both under `/api/` (D85 auth-gated by consumer convention).
    #[test]
    fn endpoint_paths_are_stable_and_distinct() {
        assert_eq!(PATH, "/api/history");
        assert_eq!(PATH_TRAJECTORY_PREFIX, "/api/history/trajectory/");
        assert!(PATH.starts_with("/api/"));
        assert!(PATH_TRAJECTORY_PREFIX.starts_with(PATH));
        // The trajectory prefix ends with a `/` so the consumer
        // can concatenate a bare pid without a separator step.
        assert!(PATH_TRAJECTORY_PREFIX.ends_with('/'));
    }

    /// Caps are the doc-locked defaults per PHASE5_HISTORY_DESIGN.md.
    #[test]
    fn caps_match_design_doc_defaults() {
        assert_eq!(TRAJECTORY_SAMPLES_PER_PID_MAX, 1800);
        assert_eq!(EVENT_ARCHIVE_MAX, 500);
        // The archive cap is 10× the live activity-feed wire cap.
        assert_eq!(
            EVENT_ARCHIVE_MAX,
            crate::limits::ACTIVITY_FEED_WIRE_MAX * 10,
            "archive cap should stay 10× the live wire cap so the \
             lazy vs live ratio is legible at a glance",
        );
    }

    /// ActivityKind reuse pin: History events serialize kind through
    /// the same three canonical strings the activity feed uses. If
    /// [`crate::activity::ActivityKind`] silently renames a variant,
    /// this test catches the drift on the History side too.
    #[test]
    fn reference_activity_kind_wire_strings_still_hold() {
        fn kind_to_str(k: ActivityKind) -> &'static str {
            match k {
                ActivityKind::Exit => "exit",
                ActivityKind::Kill => "kill",
                ActivityKind::Regression => "regression",
            }
        }
        assert_eq!(kind_to_str(ActivityKind::Exit), "exit");
        assert_eq!(kind_to_str(ActivityKind::Kill), "kill");
        assert_eq!(kind_to_str(ActivityKind::Regression), "regression");
    }

    /// Same for severity.
    #[test]
    fn reference_activity_severity_wire_strings_still_hold() {
        fn severity_to_str(s: ActivitySeverity) -> &'static str {
            match s {
                ActivitySeverity::Healthy => "healthy",
                ActivitySeverity::Attention => "attention",
                ActivitySeverity::Critical => "critical",
            }
        }
        assert_eq!(severity_to_str(ActivitySeverity::Healthy), "healthy");
        assert_eq!(severity_to_str(ActivitySeverity::Attention), "attention");
        assert_eq!(severity_to_str(ActivitySeverity::Critical), "critical");
    }

    /// The VRAM honesty convention is documented; the consumer's
    /// serde must serialize `None` as `null` (or omit), NEVER as
    /// `0`. This test is a compile-checked reminder: it constructs
    /// an unmeasured reading and asserts the discriminator matches
    /// the documented convention (the actual serde behavior is
    /// verified consumer-side, but the contract carries the rule).
    #[test]
    fn vram_unmeasured_convention_notes_null_not_zero() {
        let unmeasured: Option<u32> = None;
        let measured_zero: Option<u32> = Some(0);
        let measured_real: Option<u32> = Some(4800);
        // The three cases are distinguishable — this is the
        // discriminator the wire MUST preserve. If a consumer
        // "helpfully" collapses `None` to `Some(0)`, the operator
        // loses the ability to tell "driver unloaded" from "no VRAM
        // allocation this tick," which is exactly what
        // `VRAM_UNMEASURED` (v0.3.18) exists to distinguish.
        assert_ne!(unmeasured, measured_zero);
        assert_ne!(unmeasured, measured_real);
        assert_ne!(measured_zero, measured_real);
        // The consumer's renderer prints `VRAM_UNMEASURED` when it
        // sees `None`, NOT the number `0`.
        assert_eq!(VRAM_UNMEASURED, "—");
    }

    /// The canonical wire key strings match the JSON-standard
    /// snake_case convention (existing wire types like
    /// [`crate::host_vitals::HostVitals::thermal_zones`] use it).
    /// A silent camelCase rename would break both a JS client and
    /// any external tooling reading the endpoint.
    #[test]
    fn wire_field_keys_are_snake_case() {
        for key in [
            KEY_EVENTS,
            KEY_DEAD_PIDS,
            KEY_EVENT_KIND,
            KEY_EVENT_TIMESTAMP,
            KEY_EVENT_PID,
            KEY_EVENT_NAME,
            KEY_EVENT_SUMMARY,
            KEY_EVENT_SEVERITY,
            KEY_TRAJECTORY_SAMPLES,
            KEY_TRAJECTORY_FIRST_SAMPLE_AT,
            KEY_TRAJECTORY_LAST_SAMPLE_AT,
            KEY_SAMPLE_TIMESTAMP,
            KEY_SAMPLE_CPU_PCT,
            KEY_SAMPLE_RSS_MB,
            KEY_SAMPLE_VRAM_MB,
            KEY_DEAD_PID,
            KEY_DEAD_PID_NAME,
            KEY_DEAD_PID_MODEL,
            KEY_DEAD_PID_EXIT_TIME,
        ] {
            assert!(
                key.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "wire key {key:?} must be snake_case ASCII",
            );
            assert!(
                !key.contains("__") && !key.starts_with('_') && !key.ends_with('_'),
                "wire key {key:?} must not have leading/trailing/consecutive underscores",
            );
        }
    }

    /// Sample keys are documented in one place. If a consumer renames
    /// `cpu_pct` → `cpuPercent`, the test on the CONSUMER side that
    /// serializes into this key catches the drift, and this test
    /// pins the contract-side answer.
    #[test]
    fn sample_key_names_match_consumer_field_names() {
        assert_eq!(KEY_SAMPLE_TIMESTAMP, "timestamp");
        assert_eq!(KEY_SAMPLE_CPU_PCT, "cpu_pct");
        assert_eq!(KEY_SAMPLE_RSS_MB, "rss_mb");
        assert_eq!(KEY_SAMPLE_VRAM_MB, "vram_mb");
    }
}
