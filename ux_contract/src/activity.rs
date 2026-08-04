//! v0.3.12 CAR-21 — `ActivityState` lifted from edge_monitor's
//! Phase-2 sampler system (`src/telemetry/source.rs`).
//!
//! ## Two distinct surfaces in this module
//!
//! 1. [`ActivityState`] (v0.3.12) — per-workload activity column:
//!    a Phase-2 sampler observes one of four states for the
//!    workload it gates on (Active / Idle / Loading / NotDetected).
//!    Renders in the workloads panel's activity column.
//!
//! 2. [`ActivityKind`] + [`ActivitySeverity`] (v0.3.17 / DISPATCH 71)
//!    — activity-feed wire discriminators: the time-descending
//!    heterogeneous event log in §1 region 6 ("last 5 events" in
//!    TUI, "last 12" on web). Three event sources merged: workload
//!    exits, governor-audit (kills), Tier-1.3 regressions. The
//!    consumer projects each native event to a uniform
//!    `WireActivityEntry { kind, timestamp, pid, name, summary,
//!    severity }` and renders the same way. See `~/edge_monitor-l14/
//!    src/web/wire.rs::WireActivityEntry` for the reference
//!    consumer implementation.
//!
//! These two surfaces share the file because both are
//! "activity"-themed and the contract crate prefers a small
//! module count; semantically they are unrelated and a consumer
//! using `ActivityState` (workloads panel) does not need to know
//! about `ActivityKind` (activity feed). The doc on each item
//! states its own purpose.
//!
//! ## Wire-format convention
//!
//! Mirrors [`crate::WorkloadStatus`]: bare enums on the contract,
//! consumers convert to a wire-stable string at their
//! serialization boundary. edge_monitor's `activity_state_to_str`
//! and `activity_kind_to_str` (`src/web/wire.rs`) are the
//! reference implementations.
//!
//! Canonical strings:
//! * `ActivityState` → `"active"`, `"idle"`, `"loading"`, `"not_detected"`
//! * `ActivityKind` → `"exit"`, `"kill"`, `"regression"`
//! * `ActivitySeverity` → `"healthy"`, `"attention"`, `"critical"`
//!
//! Keeping each type bare preserves the crate's documented
//! zero-dependency stance — no `serde` derives, no `Default`
//! impl, same shape as `WorkloadStatus`.

/// Activity state of a workload, surfaced by a Phase-2 sampler.
///
/// Variants:
///
/// * [`ActivityState::Active`] — the workload is doing observable
///   work (publishing topics, running prompts, high CPU on the
///   inference loop, …).
/// * [`ActivityState::Idle`] — the workload is alive but doing no
///   observable work this tick. Distinct from `NotDetected`: the
///   sampler ran and gave a verdict.
/// * [`ActivityState::Loading`] — the workload is in a startup /
///   warm-up phase (model load, cold-start). Reserved for samplers
///   that can distinguish startup from steady-state; many
///   v1.1.x samplers do not emit this and go straight to
///   `Active` / `Idle`.
/// * [`ActivityState::NotDetected`] — the sampler ran but could
///   not determine state (no API, no shellout output, insufficient
///   samples in the rolling window, daemon outage, …). Distinct
///   from "sampler did not apply": when no sampler ever sets a
///   state for a PID, the consumer hides the column for that row
///   rather than rendering `NotDetected`.
///
/// The shape is a **bare enum** (no payload). Per-sampler "why"
/// context is debug-only and lives in the sampler's tracing
/// output, not on this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityState {
    /// Workload is doing observable work.
    Active,
    /// Workload is alive but doing no observable work this tick.
    Idle,
    /// Workload is in a startup / warm-up phase.
    Loading,
    /// Sampler ran but could not determine state.
    NotDetected,
}

/// v0.3.17 / DISPATCH 71 — activity-feed entry kind.
///
/// The §1 region 6 activity feed merges THREE event sources
/// time-descending. This enum is the bare wire discriminator that
/// tells a consumer which native shape produced the entry
/// (consumers may use it to drive a per-kind icon or color, or to
/// route an Enter into a kind-specific detail view in a future
/// post-mortem dispatch).
///
/// The three variants pin the source set against drift: a future
/// fourth source (e.g. alert raise/ack events from §4) requires a
/// CONTRACT_VERSION bump + a coordinated consumer update rather
/// than a silent enum-grow.
///
/// Canonical wire strings: `"exit"` / `"kill"` / `"regression"`.
/// See the `reference_activity_kind_wire_strings` test below for
/// the reference mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityKind {
    /// Workload exit (clean or signal-terminated). Sourced from
    /// `state.completed` (AI workloads only — non-AI exits go to
    /// the persistent JSONL log, not this feed).
    Exit,
    /// Governor-audit entry. Sourced from `state.audit` — manual
    /// `k` kills via the TUI `kill_confirm` card, plus their
    /// CANCELLED / PID-reuse-abort companion entries. Automated
    /// kills are pre-recorded in the same buffer when the
    /// (currently inert) `governor.auto_actuate` gate ever lights
    /// up; the kind is identical from the renderer's perspective.
    Kill,
    /// Tier-1.3 throughput regression. Sourced from
    /// `state.regressions`. Carries a model name + metric +
    /// delta-percent that the consumer flattens to a one-line
    /// summary.
    Regression,
}

/// v0.3.17 / DISPATCH 71 — three-band severity classifier for
/// activity-feed entries, used by both TUI (color) and web
/// (tailwind class). Mirrors the `AlertTier` shape (bare enum,
/// consumer maps to a literal at the wire boundary) so the two
/// surfaces share one taxonomy.
///
/// Server-classified at projection time so the TUI and the web
/// dashboard never disagree on whether a given event reads as
/// Healthy / Attention / Critical — the contract semantics live
/// on the producer side; the consumers are pure renderers.
///
/// Canonical wire strings: `"healthy"` / `"attention"` /
/// `"critical"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivitySeverity {
    /// Clean / successful outcome (clean exit, successful kill,
    /// information-only).
    Healthy,
    /// Worth noting but not failure (manual-kill OK, sub-critical
    /// regression). Maps to the TUI's `attention` color and the
    /// web's `text-attention` tailwind class.
    Attention,
    /// Failure or critical-band event (signal-terminated
    /// workload, failed kill, Critical-severity regression).
    Critical,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All four variants are constructible and distinct under
    /// `Eq`. Pins the variant set against a casual refactor that
    /// might add or drop one without updating downstream
    /// consumers (the canonical wire strings live downstream and
    /// must stay in sync with this set).
    #[test]
    fn all_four_variants_are_distinct() {
        let states = [
            ActivityState::Active,
            ActivityState::Idle,
            ActivityState::Loading,
            ActivityState::NotDetected,
        ];
        for (i, a) in states.iter().enumerate() {
            for (j, b) in states.iter().enumerate() {
                assert_eq!(
                    a == b,
                    i == j,
                    "variant identity must hold: {a:?} vs {b:?}",
                );
            }
        }
    }

    /// `Copy` is part of the contract: callers pass
    /// `ActivityState` by value through the renderer without
    /// reaching for clones. Pinned so a future refactor that
    /// adds a payload (which would break `Copy`) trips this
    /// test first.
    #[test]
    fn activity_state_is_copy() {
        let a = ActivityState::Active;
        let b = a; // Copy
        assert_eq!(a, b);
    }

    /// Mirrors how a consumer (edge_monitor's `activity_state_to_str`)
    /// pattern-matches each variant to a wire string. Lives here as
    /// a documented reference implementation so the wire-format
    /// convention is discoverable on the contract side without
    /// drifting from downstream consumers.
    #[test]
    fn reference_wire_strings_cover_all_variants() {
        fn reference_to_str(state: ActivityState) -> &'static str {
            match state {
                ActivityState::Active => "active",
                ActivityState::Idle => "idle",
                ActivityState::Loading => "loading",
                ActivityState::NotDetected => "not_detected",
            }
        }
        assert_eq!(reference_to_str(ActivityState::Active), "active");
        assert_eq!(reference_to_str(ActivityState::Idle), "idle");
        assert_eq!(reference_to_str(ActivityState::Loading), "loading");
        assert_eq!(
            reference_to_str(ActivityState::NotDetected),
            "not_detected",
        );
    }

    /// v0.3.17 — all three `ActivityKind` variants are
    /// constructible and distinct under `Eq`. Pins the variant
    /// set: a fourth source (alert events?) requires a
    /// CONTRACT_VERSION bump.
    #[test]
    fn all_three_activity_kind_variants_are_distinct() {
        let kinds = [
            ActivityKind::Exit,
            ActivityKind::Kill,
            ActivityKind::Regression,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for (j, b) in kinds.iter().enumerate() {
                assert_eq!(
                    a == b,
                    i == j,
                    "ActivityKind variant identity must hold: {a:?} vs {b:?}",
                );
            }
        }
    }

    /// `Copy` is part of the contract for both new v0.3.17 enums —
    /// callers pass them by value through the renderer. Pinned so
    /// a future refactor adding a payload (which would break
    /// `Copy`) trips this test first.
    #[test]
    fn activity_kind_and_severity_are_copy() {
        let k = ActivityKind::Kill;
        let k2 = k; // Copy
        assert_eq!(k, k2);
        let s = ActivitySeverity::Attention;
        let s2 = s; // Copy
        assert_eq!(s, s2);
    }

    /// Reference wire-string mapping for the new v0.3.17 enums.
    /// Mirrors the same documented-reference-implementation
    /// pattern `reference_wire_strings_cover_all_variants` uses for
    /// `ActivityState`: the canonical strings live downstream
    /// (`src/web/wire.rs::{activity_kind_to_str, activity_severity_to_str}`)
    /// but this test pins the expected output so a downstream
    /// rename surfaces here too.
    #[test]
    fn reference_activity_kind_wire_strings() {
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

    #[test]
    fn reference_activity_severity_wire_strings() {
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
}
